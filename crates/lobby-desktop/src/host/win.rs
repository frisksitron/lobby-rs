#![allow(unsafe_code)]

use std::{os::windows::io::RawHandle, time::Duration};

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, RECT, WAIT_OBJECT_0},
    System::{
        Pipes::GetNamedPipeServerProcessId,
        RemoteDesktop::ProcessIdToSessionId,
        SystemInformation::GetTickCount,
        Threading::{GetCurrentProcessId, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    },
    UI::{
        Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
        WindowsAndMessaging::{
            AllowSetForegroundWindow, GetClientRect, GetSystemMetrics, GetWindowRect, HWND_BOTTOM,
            SM_XVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
            SWP_SHOWWINDOW, SetWindowPos,
        },
    },
};

pub(super) fn idle_time() -> Option<Duration> {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a live LASTINPUTINFO with its size set, as the call requires.
    let ok = unsafe { GetLastInputInfo(&raw mut info) }.as_bool();
    if !ok {
        return None;
    }
    // SAFETY: no arguments; it reads the system tick count.
    let now = unsafe { GetTickCount() };
    // Both are 32-bit millisecond tick counts; the subtraction wraps with them (every 49.7 days).
    Some(Duration::from_millis(u64::from(
        now.wrapping_sub(info.dwTime),
    )))
}

/// Shows the window just past the left edge of the virtual screen, at the bottom of the z-order and
/// without activating it: drawn and capturable, never in the person's way.
pub(super) fn park_offscreen(hwnd: isize) -> bool {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let mut rect = RECT::default();
    // SAFETY: `hwnd` is the live window GPUI created on this thread; `rect` is a valid RECT.
    if unsafe { GetWindowRect(hwnd, &raw mut rect) }.is_err() {
        return false;
    }
    // SAFETY: no pointers; it reads a system metric.
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let x = left - (rect.right - rect.left) - 64;
    // SAFETY: as above; the flags keep the size and never activate the window.
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            x,
            rect.top,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
    .is_ok()
}

pub(super) fn resize_client(hwnd: isize, width: i32, height: i32) -> bool {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let (mut outer, mut inner) = (RECT::default(), RECT::default());
    // SAFETY: `hwnd` is the live window GPUI created on this thread; both RECTs are valid.
    let read =
        unsafe { GetWindowRect(hwnd, &raw mut outer).and(GetClientRect(hwnd, &raw mut inner)) };
    if read.is_err() {
        return false;
    }
    let frame_w = (outer.right - outer.left) - (inner.right - inner.left);
    let frame_h = (outer.bottom - outer.top) - (inner.bottom - inner.top);
    // SAFETY: as above; the flags keep the place and the z-order and never activate it.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width + frame_w,
            height + frame_h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .is_ok()
}

/// The Windows session (one per signed-in user, and per Remote Desktop login) this process runs
/// in; 0 when Windows will not say.
pub(super) fn session_id() -> u32 {
    let mut session = 0;
    // SAFETY: the process id comes from the call with no arguments; `session` is a valid out
    // pointer.
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &raw mut session) };
    session
}

/// The process serving the named pipe `client` is connected to.
pub(super) fn pipe_server_process(client: RawHandle) -> Option<u32> {
    let mut pid = 0;
    // SAFETY: `client` is the live handle of a connected pipe client, which the caller holds for
    // the call; `pid` is a valid out pointer.
    unsafe { GetNamedPipeServerProcessId(HANDLE(client), &raw mut pid) }
        .ok()
        .map(|()| pid)
}

/// Waits up to `timeout` for process `pid` to exit; true once it has, or if it is already gone.
pub(super) fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    // SAFETY: only a process id; the handle is closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
        return true;
    };
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: `process` is the live handle opened above.
    let exited = unsafe { WaitForSingleObject(process, ms) } == WAIT_OBJECT_0;
    // SAFETY: closed once, here.
    let _ = unsafe { CloseHandle(process) };
    exited
}

/// Lets `pid` bring its window to the front even though this process was not the last to get
/// input: the second launch hands its right to the first.
pub(super) fn allow_foreground_for(pid: u32) -> bool {
    // SAFETY: only a process id.
    unsafe { AllowSetForegroundWindow(pid) }.is_ok()
}

pub(super) use overlay::*;

/// The Sharer's ink overlay: a window over a whole monitor, or just above one window, that clicks
/// pass through, that never takes the focus, and that captures leave out.
mod overlay {
    use std::cell::RefCell;

    use tokio::sync::mpsc::UnboundedSender;
    use windows::Win32::{
        Foundation::{COLORREF, HWND, POINT, RECT},
        Graphics::{
            Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::{
                ClientToScreen, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONULL, MONITORINFO,
                MonitorFromWindow,
            },
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            HiDpi::{
                AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                GetWindowDpiAwarenessContext, SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_LOCATIONCHANGE,
                EVENT_OBJECT_REORDER, EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED,
                EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
                GW_HWNDPREV, GWL_EXSTYLE, GWL_STYLE, GetWindow, GetWindowDisplayAffinity,
                GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, HWND_NOTOPMOST,
                HWND_TOPMOST, IsIconic, IsWindow, IsWindowVisible, LWA_ALPHA, OBJID_WINDOW,
                SET_WINDOW_POS_FLAGS, SW_HIDE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
                SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetLayeredWindowAttributes,
                SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow,
                WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WINEVENT_OUTOFCONTEXT, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    };

    use crate::host::{OverlayFault, OverlayTarget};

    fn hwnd(raw: isize) -> HWND {
        HWND(raw as *mut core::ffi::c_void)
    }

    /// Runs `open` with this thread Per-Monitor V2 DPI aware, so a window it creates is, whatever
    /// the process is. The thread goes back to what it was; the process never changes.
    pub fn per_monitor_v2<T>(open: impl FnOnce() -> T) -> T {
        // SAFETY: only a context constant; the call returns the thread's context before it.
        let before =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let opened = open();
        if !before.0.is_null() {
            // SAFETY: the context the same call returned for this thread.
            unsafe { SetThreadDpiAwarenessContext(before) };
        }
        opened
    }

    /// Makes the window GPUI created into the overlay over `target`, hidden. Each step must
    /// hold, or ink would show in the capture or get in the person's way.
    pub fn make_overlay(raw: isize, target: OverlayTarget) -> Result<(), OverlayFault> {
        let hwnd = hwnd(raw);
        // SAFETY: `hwnd` is the live window GPUI created on this thread.
        let dpi = unsafe { GetWindowDpiAwarenessContext(hwnd) };
        // SAFETY: two context values, compared.
        if !unsafe { AreDpiAwarenessContextsEqual(dpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            .as_bool()
        {
            return Err(OverlayFault::Dpi);
        }
        // SAFETY: as above; it reads the extended style.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        // GPUI draws with per-pixel alpha only through DirectComposition, which it marks so.
        if style & WS_EX_NOREDIRECTIONBITMAP.0 as isize == 0 {
            return Err(OverlayFault::Alpha);
        }
        let through = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
        // SAFETY: as above; it adds style bits to the window's own.
        unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | through.0 as isize) };
        // GPUI's pop-up keeps a resizing frame at its sides, which would shift the ink off the
        // shared pixels; a bare pop-up's client area is the whole window.
        // SAFETY: as above; the window is hidden, and `place` applies the frame change.
        unsafe { SetWindowLongPtrW(hwnd, GWL_STYLE, WS_POPUP.0 as isize) };
        // A layered window shows nothing until its alpha is set.
        // SAFETY: as above.
        unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) }
            .map_err(|_| OverlayFault::Styles)?;
        // SAFETY: as above; it reads the extended style back.
        let set = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        if set & through.0 as isize != through.0 as isize {
            return Err(OverlayFault::Styles);
        }
        if !exclude_from_capture(raw, true) {
            return Err(OverlayFault::Exclusion);
        }
        if !place(hwnd, target, false) {
            return Err(OverlayFault::SourceGone);
        }
        Ok(())
    }

    /// Keeps the window out of every capture of the screen, or lets it back in. True when
    /// Windows reads back what was asked.
    pub fn exclude_from_capture(raw: isize, excluded: bool) -> bool {
        let hwnd = hwnd(raw);
        let want = if excluded {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        // SAFETY: `hwnd` is a live window of this thread.
        if unsafe { SetWindowDisplayAffinity(hwnd, want) }.is_err() {
            return false;
        }
        let mut read = 0;
        // SAFETY: as above; `read` is a valid out pointer.
        let ok = unsafe { GetWindowDisplayAffinity(hwnd, &raw mut read) }.is_ok();
        ok && read == want.0
    }

    /// Shows the overlay over `target` without activating it, or hides it. Over a window it
    /// stays hidden while the window is minimised, cloaked or hidden. False when the target is
    /// gone.
    pub fn show_overlay(raw: isize, target: OverlayTarget, shown: bool) -> bool {
        let hwnd = hwnd(raw);
        if shown {
            place(hwnd, target, true)
        } else {
            hide(hwnd);
            true
        }
    }

    fn hide(hwnd: HWND) {
        // SAFETY: `hwnd` is a live window of this thread; the result is the old visibility.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }

    /// Covers `target` with the window, in physical pixels: a whole monitor, topmost, or a window
    /// as captures see it, just above that window. False when the target is gone.
    fn place(hwnd: HWND, target: OverlayTarget, show: bool) -> bool {
        let shown = if show {
            SWP_SHOWWINDOW
        } else {
            SET_WINDOW_POS_FLAGS::default()
        };
        match target {
            OverlayTarget::Monitor(monitor) => {
                let Some(rect) = monitor_rect(monitor) else {
                    return false;
                };
                move_to(hwnd, rect, Some(HWND_TOPMOST), shown)
            }
            OverlayTarget::Window(raw) => {
                let window = self::hwnd(raw);
                // SAFETY: only a handle; a stale one answers false.
                if !unsafe { IsWindow(Some(window)) }.as_bool() {
                    return false;
                }
                if !on_screen(window) {
                    hide(hwnd);
                    return true;
                }
                let Some(rect) = frame(window) else {
                    return false;
                };
                let after = above(window, hwnd);
                // Windows ignores NOTOPMOST for a window that is not topmost, and HWND_TOP leaves
                // it under a window raised without activation: it goes up through the topmost
                // band.
                if after == Some(HWND_NOTOPMOST) && !topmost(hwnd) {
                    // SAFETY: `hwnd` is a live window of this thread; the flags keep its place
                    // and never activate it.
                    let _ = unsafe {
                        SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        )
                    };
                }
                // Most window events, and every periodic check, change nothing it cares about.
                if after.is_none() && window_rect(hwnd) == Some(rect) && (!show || visible(hwnd)) {
                    return true;
                }
                move_to(hwnd, rect, after, shown)
            }
        }
    }

    fn move_to(hwnd: HWND, rect: RECT, after: Option<HWND>, shown: SET_WINDOW_POS_FLAGS) -> bool {
        let order = if after.is_none() {
            SWP_NOZORDER
        } else {
            SET_WINDOW_POS_FLAGS::default()
        };
        let flags = SWP_NOACTIVATE | SWP_FRAMECHANGED | shown | order;
        // Twice: crossing to a monitor of another DPI, GPUI resizes the window to what Windows
        // suggests, which keeps the old logical size; the second move is within one DPI.
        (0..2).all(|_| {
            // SAFETY: `hwnd` is a live window of this thread; the flags never activate it.
            unsafe {
                SetWindowPos(
                    hwnd,
                    after,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    flags,
                )
            }
            .is_ok()
        })
    }

    fn monitor_rect(monitor: isize) -> Option<RECT> {
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is a live MONITORINFO with its size set; a stale handle fails the call.
        let known = unsafe { GetMonitorInfoW(HMONITOR(monitor as *mut _), &raw mut info) };
        known.as_bool().then_some(info.rcMonitor)
    }

    /// The window as a capture of it sees it: its frame without the shadow, in physical pixels.
    fn frame(window: HWND) -> Option<RECT> {
        let mut rect = RECT::default();
        // SAFETY: `rect` is a valid RECT of the size given.
        unsafe {
            DwmGetWindowAttribute(
                window,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&raw mut rect).cast(),
                size_of::<RECT>() as u32,
            )
        }
        .ok()
        .map(|()| rect)
    }

    /// Whether the window is to be seen: shown, not minimised and not cloaked (on another
    /// virtual desktop, or a suspended app's).
    fn on_screen(window: HWND) -> bool {
        let mut cloaked = 0u32;
        // SAFETY: `cloaked` is a valid u32 of the size given.
        let read = unsafe {
            DwmGetWindowAttribute(
                window,
                DWMWA_CLOAKED,
                (&raw mut cloaked).cast(),
                size_of::<u32>() as u32,
            )
        };
        // SAFETY: only a handle.
        let iconic = unsafe { IsIconic(window) }.as_bool();
        visible(window) && !iconic && (read.is_err() || cloaked == 0)
    }

    fn visible(window: HWND) -> bool {
        // SAFETY: only a handle; a stale one answers false.
        unsafe { IsWindowVisible(window) }.as_bool()
    }

    fn window_rect(window: HWND) -> Option<RECT> {
        let mut rect = RECT::default();
        // SAFETY: `rect` is a valid RECT; a stale handle fails the call.
        unsafe { GetWindowRect(window, &raw mut rect) }
            .ok()
            .map(|()| rect)
    }

    /// What to put the overlay after so that it sits just above `window`, in the same band
    /// (topmost or not) as the window; `None` where it does.
    fn above(window: HWND, overlay: HWND) -> Option<HWND> {
        let band = topmost(window);
        // SAFETY: only a handle; there is no window above the top one, which is an error.
        match unsafe { GetWindow(window, GW_HWNDPREV) } {
            Ok(prev) if prev == overlay && topmost(overlay) == band => None,
            Ok(prev) if prev != overlay && topmost(prev) == band => Some(prev),
            // The window tops its band; put after a topmost window, the overlay would stay in
            // the topmost one, over every other app.
            _ if band => Some(HWND_TOPMOST),
            _ => Some(HWND_NOTOPMOST),
        }
    }

    fn topmost(window: HWND) -> bool {
        // SAFETY: only a handle; it reads the extended style.
        let style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
        style & WS_EX_TOPMOST.0 as isize != 0
    }

    thread_local! {
        /// The windows followed on this thread, each with whom to wake.
        static FOLLOWED: RefCell<Vec<(isize, UnboundedSender<()>)>> =
            const { RefCell::new(Vec::new()) };
    }

    /// Hooks on another app's window that wake `moved` when it moves, changes size, is minimised
    /// or restored, hidden or shown, cloaked or uncloaked, or brought forward. Windows calls the
    /// hooks from this thread's message loop. Dropping it unhooks, on the thread that hooked.
    pub struct WindowWatch {
        hooks: Vec<HWINEVENTHOOK>,
        moved: UnboundedSender<()>,
    }

    impl WindowWatch {
        /// `None` when the window is gone or Windows sets no hook.
        pub fn start(window: isize, moved: UnboundedSender<()>) -> Option<WindowWatch> {
            let mut process = 0;
            // SAFETY: a stale handle answers 0; `process` is a valid out pointer.
            let thread = unsafe { GetWindowThreadProcessId(hwnd(window), Some(&raw mut process)) };
            if thread == 0 {
                return None;
            }
            FOLLOWED.with_borrow_mut(|followed| followed.push((window, moved.clone())));
            let mut watch = WindowWatch {
                hooks: Vec::new(),
                moved,
            };
            let events = [
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
                // Shown, hidden and reordered.
                (EVENT_OBJECT_SHOW, EVENT_OBJECT_REORDER),
                (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
                (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
            ];
            for (first, last) in events {
                // SAFETY: `on_event` lives as long as the program; out of context, Windows calls
                // it on this thread, from its message loop, for the window's process only.
                let hook = unsafe {
                    SetWinEventHook(
                        first,
                        last,
                        None,
                        Some(on_event),
                        process,
                        0,
                        WINEVENT_OUTOFCONTEXT,
                    )
                };
                if hook.is_invalid() {
                    return None;
                }
                watch.hooks.push(hook);
            }
            Some(watch)
        }
    }

    impl Drop for WindowWatch {
        fn drop(&mut self) {
            for hook in self.hooks.drain(..) {
                // SAFETY: a hook this watch set, on this thread.
                let _ = unsafe { UnhookWinEvent(hook) };
            }
            FOLLOWED.with_borrow_mut(|followed| {
                followed.retain(|(_, moved)| !moved.same_channel(&self.moved));
            });
        }
    }

    unsafe extern "system" fn on_event(
        _hook: HWINEVENTHOOK,
        event: u32,
        window: HWND,
        object: i32,
        child: i32,
        _thread: u32,
        _time: u32,
    ) {
        let whole = object == OBJID_WINDOW.0 && child == CHILDID_SELF as i32;
        // A reorder names the container whose children moved, not the window.
        if !whole && event != EVENT_OBJECT_REORDER {
            return;
        }
        let window = window.0 as isize;
        FOLLOWED.with_borrow(|followed| {
            for (followed, moved) in followed {
                if *followed == window || event == EVENT_OBJECT_REORDER {
                    let _ = moved.send(());
                }
            }
        });
    }

    /// Where the window's client area starts on the virtual screen, in physical pixels.
    pub fn client_origin(raw: isize) -> Option<(i32, i32)> {
        let mut at = POINT::default();
        // SAFETY: `at` is a valid POINT; a stale handle fails the call.
        unsafe { ClientToScreen(hwnd(raw), &raw mut at) }
            .as_bool()
            .then_some((at.x, at.y))
    }

    /// Whether most of the window is on `monitor`.
    pub fn on_monitor(raw: isize, monitor: isize) -> bool {
        // SAFETY: a stale handle answers null.
        let on = unsafe { MonitorFromWindow(hwnd(raw), MONITOR_DEFAULTTONULL) };
        !on.is_invalid() && on.0 as isize == monitor
    }

    /// The window a click at `x`, `y` (physical pixels) would land on.
    #[cfg(feature = "automation")]
    pub fn window_at(x: i32, y: i32) -> Option<isize> {
        // SAFETY: only a point.
        let at =
            unsafe { windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(POINT { x, y }) };
        (!at.is_invalid()).then_some(at.0 as isize)
    }

    /// The window with the keyboard focus, of any process.
    #[cfg(feature = "automation")]
    pub fn foreground() -> Option<isize> {
        // SAFETY: no arguments.
        let at = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
        (!at.is_invalid()).then_some(at.0 as isize)
    }

    /// Where a window stands, for the overlay check: its frame as captures see it, whether it
    /// is shown and topmost, and the window just above it.
    #[cfg(feature = "automation")]
    pub fn window_state(raw: isize) -> Option<crate::host::WindowState> {
        let window = hwnd(raw);
        let RECT {
            left,
            top,
            right,
            bottom,
        } = frame(window)?;
        // SAFETY: only a handle; there is no window above the top one.
        let above = unsafe { GetWindow(window, GW_HWNDPREV) }.ok();
        Some(crate::host::WindowState {
            frame: (left, top, right - left, bottom - top),
            shown: visible(window),
            topmost: topmost(window),
            above: above.map(|w| w.0 as isize),
        })
    }

    /// Moves another app's window, or minimises and restores it, without activating it.
    #[cfg(feature = "automation")]
    pub fn drive_window(raw: isize, to: crate::host::DriveWindow) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE};

        use crate::host::DriveWindow;
        let window = hwnd(raw);
        match to {
            DriveWindow::Place(x, y, width, height) => {
                // SAFETY: only a handle; the flags keep the z-order and never activate it.
                unsafe {
                    SetWindowPos(
                        window,
                        None,
                        x,
                        y,
                        width,
                        height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                }
                .is_ok()
            }
            DriveWindow::Minimise => {
                // SAFETY: only a handle; the result is the old visibility.
                let _ = unsafe { ShowWindow(window, SW_SHOWMINNOACTIVE) };
                true
            }
            DriveWindow::Restore => {
                // SAFETY: as above.
                let _ = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };
                true
            }
            DriveWindow::Raise => {
                use windows::Win32::UI::WindowsAndMessaging::HWND_TOP;
                // SAFETY: only a handle; the flags keep its place and never activate it.
                unsafe {
                    SetWindowPos(
                        window,
                        Some(HWND_TOP),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    )
                }
                .is_ok()
            }
        }
    }
}
