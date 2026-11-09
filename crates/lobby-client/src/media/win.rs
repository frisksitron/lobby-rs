//! Win32 calls that windows-capture does not wrap. Screen Share's Direct3D work has its own
//! `unsafe` module, [`super::gpu`].
#![allow(unsafe_code)]

use std::ffi::c_void;

use tracing::warn;
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, MONITOR_DEFAULTTONULL,
            MonitorFromWindow, ReleaseDC, SelectObject,
        },
    },
    Media::{
        Audio::{
            AUDIO_VOLUME_NOTIFICATION_DATA,
            Endpoints::{
                IAudioEndpointVolume, IAudioEndpointVolumeCallback,
                IAudioEndpointVolumeCallback_Impl,
            },
            IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eCommunications, eRender,
        },
        TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod,
    },
    Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow},
    System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    },
    UI::{
        Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState},
        WindowsAndMessaging::{
            GetForegroundWindow, GetWindowDisplayAffinity, GetWindowRect, IsIconic,
            PW_RENDERFULLCONTENT,
        },
    },
};
use windows::core::{GUID, HSTRING, implement};

/// The window is cloaked by DWM: visible to `IsWindowVisible` yet not on screen (a suspended
/// UWP app, a window on another virtual desktop, shell surfaces such as "Windows Input
/// Experience"). Capturing one shares nothing useful, so the picker leaves it out.
pub(crate) fn is_cloaked(hwnd: *mut c_void) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: the out pointer is a live `u32`, the size DWMWA_CLOAKED writes; a stale handle
    // only makes the call fail.
    let read = unsafe {
        DwmGetWindowAttribute(
            HWND(hwnd),
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
    };
    read.is_ok() && cloaked != 0
}

/// The window keeps itself out of captures (`SetWindowDisplayAffinity`, as Signal does): Windows
/// Graphics Capture starts on it and never delivers a frame. Any affinity counts: `WDA_MONITOR`
/// blacks the window out, `WDA_EXCLUDEFROMCAPTURE` leaves it out.
pub(crate) fn blocks_capture(hwnd: *mut c_void) -> bool {
    let mut affinity: u32 = 0;
    // SAFETY: the out pointer is a live `u32`; a stale handle only makes the call fail.
    let read = unsafe { GetWindowDisplayAffinity(HWND(hwnd), &raw mut affinity) };
    read.is_ok() && affinity != 0
}

/// The window is minimized: Windows Graphics Capture never delivers a frame of one.
pub(crate) fn is_minimized(hwnd: *mut c_void) -> bool {
    // SAFETY: a stale handle only makes the call answer false.
    unsafe { IsIconic(HWND(hwnd)) }.as_bool()
}

/// The foreground window and its monitor, while Windows says a Direct3D game holds the display in
/// exclusive fullscreen. It also says so of some games that are not, so a capture that still
/// makes pictures is fine whatever this answers.
pub(crate) fn exclusive_fullscreen() -> Option<(isize, isize)> {
    // SAFETY: plain calls that borrow nothing; a window that is gone only gives a null monitor.
    unsafe {
        if SHQueryUserNotificationState().ok()? != QUNS_RUNNING_D3D_FULL_SCREEN {
            return None;
        }
        let window = GetForegroundWindow();
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONULL);
        Some((window.0 as isize, monitor.0 as isize))
    }
}

/// A window's picture as `width * height * 4` bytes of BGRA without row padding, drawn by the
/// window itself (`PrintWindow` with `PW_RENDERFULLCONTENT`). The alpha byte is not meaningful.
pub(crate) fn print_window(hwnd: *mut c_void) -> Option<(u32, u32, Vec<u8>)> {
    let hwnd = HWND(hwnd);
    let mut rect = RECT::default();
    // SAFETY: the out pointer is a live RECT; a stale handle only makes the call fail.
    unsafe { GetWindowRect(hwnd, &raw mut rect) }.ok()?;
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    if width <= 0 || height <= 0 {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: each GDI object made below is selected out and deleted before the function
    // returns, and the DIB section's pixels are read only while its bitmap is alive.
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let mut bits: *mut c_void = std::ptr::null_mut();
        let picture = CreateDIBSection(
            Some(dc),
            &raw const info,
            DIB_RGB_COLORS,
            &raw mut bits,
            None,
            0,
        );
        let copied = match picture {
            Ok(bitmap) if !bits.is_null() => {
                let previous = SelectObject(dc, bitmap.into());
                let printed = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));
                let _ = GdiFlush();
                let pixels = printed.as_bool().then(|| {
                    let len = (width as usize) * (height as usize) * 4;
                    std::slice::from_raw_parts(bits.cast::<u8>(), len).to_vec()
                });
                SelectObject(dc, previous);
                let _ = DeleteObject(bitmap.into());
                pixels
            }
            _ => None,
        };
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        copied.map(|bgra| (width as u32, height as u32, bgra))
    }
}

/// While it lives, the process's timed waits end within about a millisecond, not on Windows'
/// 15.6 ms tick. A Sharer needs it: str0m sends a probe's padding in 2 ms bursts, one per wake
/// of the transport's loop, so on the tick a probe sent and read 20-35 % of what it aimed at.
pub(crate) struct FineTimers {
    granted: bool,
}

impl FineTimers {
    const PERIOD_MS: u32 = 1;

    pub(crate) fn hold() -> FineTimers {
        // SAFETY: a plain call; a granted period is ended once, on drop.
        let granted = unsafe { timeBeginPeriod(Self::PERIOD_MS) } == TIMERR_NOERROR;
        if !granted {
            warn!("a 1 ms timer period refused: the share's probes read the uplink low");
        }
        FineTimers { granted }
    }
}

impl Drop for FineTimers {
    fn drop(&mut self) {
        if self.granted {
            // SAFETY: ends the period `hold` began.
            unsafe { timeEndPeriod(Self::PERIOD_MS) };
        }
    }
}

/// COM on this thread, once. A mode already set on it (cpal's) is fine: the objects this module
/// makes are free-threaded.
fn com() {
    thread_local! {
        static COM: () = {
            // SAFETY: no reserved argument.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        };
    }
    COM.with(|()| ());
}

/// The id of the microphone (or the speakers) Windows uses for calls, which cpal cannot name:
/// it only knows the console default. `None` where there is none.
pub(crate) fn communications_endpoint(capture: bool) -> Option<String> {
    com();
    let flow = if capture { eCapture } else { eRender };
    // SAFETY: COM is initialised on this thread, and the id string Windows allocates is copied
    // and freed before the function returns.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator
            .GetDefaultAudioEndpoint(flow, eCommunications)
            .ok()?;
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0.cast_const().cast()));
        text
    }
}

/// Lobby's own volume changes carry this, so a change notice can tell them from the OS's.
static OWN_CHANGE: GUID = GUID::from_u128(0x5b1e_7c2a_4d3f_4a8e_9c61_0f2d_8e4b_a713);

/// An endpoint's master volume, the one Windows' Sound settings show, and a callback that hears
/// each change of it. Dropping it stops the callback.
pub(crate) struct EndpointVolume {
    volume: IAudioEndpointVolume,
    callback: IAudioEndpointVolumeCallback,
}

impl EndpointVolume {
    /// The endpoint whose IMMDevice id is `id`. `changed` gets the new level, from 0 to 1, and
    /// whether Lobby made the change, on a thread of Windows'.
    pub(crate) fn open(
        id: &str,
        changed: impl Fn(f32, bool) + Send + Sync + 'static,
    ) -> windows::core::Result<EndpointVolume> {
        com();
        // SAFETY: COM is initialised on this thread; the interfaces are reference counted, and
        // the callback stays registered only while `EndpointVolume` holds both.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator.GetDevice(&HSTRING::from(id))?;
            let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            let callback: IAudioEndpointVolumeCallback = VolumeChanged {
                changed: Box::new(changed),
            }
            .into();
            volume.RegisterControlChangeNotify(&callback)?;
            Ok(EndpointVolume { volume, callback })
        }
    }

    pub(crate) fn level(&self) -> windows::core::Result<f32> {
        // SAFETY: a plain call on a live interface.
        unsafe { self.volume.GetMasterVolumeLevelScalar() }
    }

    pub(crate) fn set(&self, level: f32) -> windows::core::Result<()> {
        // SAFETY: the context pointer is a static GUID.
        unsafe {
            self.volume
                .SetMasterVolumeLevelScalar(level, &raw const OWN_CHANGE)
        }
    }
}

impl Drop for EndpointVolume {
    fn drop(&mut self) {
        // SAFETY: unregisters the callback `open` registered on the same interface.
        let _ = unsafe { self.volume.UnregisterControlChangeNotify(&self.callback) };
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeChanged {
    changed: Box<dyn Fn(f32, bool) + Send + Sync>,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeChanged_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        // SAFETY: Windows passes a notification that lives for the length of the call.
        if let Some(data) = unsafe { data.as_ref() } {
            (self.changed)(data.fMasterVolume, data.guidEventContext == OWN_CHANGE);
        }
        Ok(())
    }
}
