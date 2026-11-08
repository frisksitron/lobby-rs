//! The notification-area icon and the OS window's hide and show. The unsafe Win32 calls of the
//! desktop host's tray; `win.rs` keeps the rest.
#![allow(unsafe_code)]

use std::sync::Once;

use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_STATE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
                NIN_SELECT, NIS_HIDDEN, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, GWLP_USERDATA, GetSystemMetrics, GetWindowLongPtrW, HICON,
                IDI_APPLICATION, IMAGE_ICON, IsIconic, LR_SHARED, LoadIconW, LoadImageW, MF_STRING,
                PostMessageW, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON,
                SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE, SetForegroundWindow,
                SetWindowLongPtrW, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
                TrackPopupMenu, WM_APP, WM_CONTEXTMENU, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW,
                WS_POPUP,
            },
        },
    },
    core::{PCWSTR, w},
};

use super::HostEvent;

fn hwnd_of(hwnd: isize) -> HWND {
    HWND(hwnd as *mut core::ffi::c_void)
}

pub(super) fn hide_window(hwnd: isize) {
    // SAFETY: `hwnd` is the live window GPUI created on this thread; the call only changes its
    // visibility. It sends messages to the window synchronously, so the caller is not in an update.
    let _ = unsafe { ShowWindow(hwnd_of(hwnd), SW_HIDE) };
}

/// Without `activate` the window comes back where it was, under whatever the person is using.
pub(super) fn show_window(hwnd: isize, activate: bool) {
    let hwnd = hwnd_of(hwnd);
    // SAFETY: as in `hide_window`.
    unsafe {
        if !activate {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            return;
        }
        let command = if IsIconic(hwnd).as_bool() {
            SW_RESTORE
        } else {
            SW_SHOW
        };
        let _ = ShowWindow(hwnd, command);
        // Refused when the process may not take the foreground (a tray click and a second launch
        // allow it); the taskbar button flashes instead.
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Enter or Space on the focused icon: `NIN_SELECT | NINF_KEY` in the headers, missing from the
/// crate.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const CALLBACK_MESSAGE: u32 = WM_APP + 1;
const ICON_ID: u32 = 1;
const MENU_SHOW: usize = 1;
const MENU_QUIT: usize = 2;
/// The icon resource `build.rs` embeds; GPUI uses the same id for the window's.
const ICON_RESOURCE: usize = 1;
const CLASS: PCWSTR = w!("LobbyTrayWindow");

/// A hidden window that owns the icon. It is a plain top-level window that is never shown, not a
/// message-only one: only top-level windows are sent the `TaskbarCreated` broadcast that says
/// Explorer restarted and the icon has to be added again.
pub(in crate::host) struct TrayIcon {
    hwnd: HWND,
    state: *mut State,
}

struct State {
    events: UnboundedSender<HostEvent>,
    icon: HICON,
    tip: [u16; 128],
    hidden: bool,
    taskbar_created: u32,
}

impl TrayIcon {
    pub(in crate::host) fn add(
        tooltip: &str,
        shown: bool,
        events: UnboundedSender<HostEvent>,
    ) -> Option<Self> {
        register_class();
        let mut tip = [0u16; 128];
        for (slot, unit) in tip.iter_mut().zip(tooltip.encode_utf16().take(127)) {
            *slot = unit;
        }
        // SAFETY: the name is a valid NUL-terminated string.
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let state = Box::into_raw(Box::new(State {
            events,
            icon: load_icon(),
            tip,
            hidden: !shown,
            taskbar_created,
        }));
        // SAFETY: the class is registered; the window has no parent and takes no input.
        let created = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                w!("Lobby tray"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                module(),
                None,
            )
        };
        let Ok(hwnd) = created else {
            warn!("cannot create the tray's window");
            // SAFETY: `state` came from `Box::into_raw` above and nothing else holds it.
            drop(unsafe { Box::from_raw(state) });
            return None;
        };
        // SAFETY: the window was made on this thread, and `state` outlives it: `Drop` clears the
        // pointer before freeing it. `state` is live and only this thread touches it.
        let added = unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            add_icon(hwnd, &*state)
        };
        let icon = Self { hwnd, state };
        if added {
            Some(icon)
        } else {
            warn!("the notification area refused the icon");
            None
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // SAFETY: `state` is the Box leaked in `add`, freed only here; the window belongs to this
        // thread, which is the one dropping the icon.
        unsafe {
            let mut data = base_data(self.hwnd);
            let _ = Shell_NotifyIconW(NIM_DELETE, &raw mut data);
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            drop(Box::from_raw(self.state));
        }
    }
}

fn module() -> Option<HINSTANCE> {
    // SAFETY: no pointer arguments: the handle of this executable.
    unsafe { GetModuleHandleW(None) }.ok().map(Into::into)
}

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module().unwrap_or_default(),
            lpszClassName: CLASS,
            ..WNDCLASSW::default()
        };
        // SAFETY: `class` is fully initialised and its name is static.
        unsafe { RegisterClassW(&class) };
    });
}

/// The exe's own icon at the notification area's size; the stock one when the resource is missing
/// (a build without it).
fn load_icon() -> HICON {
    // SAFETY: the module handle is this executable's; the resource id is passed as a pointer value,
    // as `MAKEINTRESOURCE` does; a shared icon is never freed.
    unsafe {
        let (w, h) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        if let Some(module) = module()
            && let Ok(handle) = LoadImageW(
                Some(module),
                PCWSTR(ICON_RESOURCE as *const u16),
                IMAGE_ICON,
                w,
                h,
                LR_SHARED,
            )
        {
            return HICON(handle.0);
        }
        LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
    }
}

fn base_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..NOTIFYICONDATAW::default()
    }
}

/// Adds the icon and asks for version 4 messages (the click's anchor point, keyboard selection).
///
/// # Safety
/// Called on the thread that owns `hwnd`.
unsafe fn add_icon(hwnd: HWND, state: &State) -> bool {
    let mut data = NOTIFYICONDATAW {
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_STATE,
        uCallbackMessage: CALLBACK_MESSAGE,
        hIcon: state.icon,
        szTip: state.tip,
        dwState: if state.hidden {
            NIS_HIDDEN
        } else {
            Default::default()
        },
        dwStateMask: NIS_HIDDEN,
        ..base_data(hwnd)
    };
    // SAFETY: `data` is fully initialised with its size set.
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &raw mut data).as_bool() {
            return false;
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        Shell_NotifyIconW(NIM_SETVERSION, &raw mut data).as_bool()
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the pointer is the live `State` set by `add` (null before it and after `Drop`), and
    // this runs on the thread that owns it.
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State).as_ref() };
    let Some(state) = state else {
        // SAFETY: the arguments are the ones this procedure was called with.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        CALLBACK_MESSAGE => {
            match lparam.0 as u32 & 0xFFFF {
                NIN_SELECT | NIN_KEYSELECT => {
                    let _ = state.events.send(HostEvent::Show);
                }
                WM_CONTEXTMENU => {
                    // Version 4 passes the anchor in the low and high words of `wparam`.
                    let x = wparam.0 as u32 as i16 as i32;
                    let y = (wparam.0 as u32 >> 16) as i16 as i32;
                    if let Some(event) = menu(hwnd, x, y) {
                        let _ = state.events.send(event);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        m if m == state.taskbar_created => {
            // SAFETY: on the owning thread, with the live state.
            unsafe { add_icon(hwnd, state) };
            LRESULT(0)
        }
        // SAFETY: the arguments are the ones this procedure was called with.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Shows the menu at the click and waits for the choice. The window must be the foreground one or
/// the menu stays open after a click elsewhere; the `WM_NULL` lets it close.
fn menu(hwnd: HWND, x: i32, y: i32) -> Option<HostEvent> {
    // SAFETY: the menu is made, shown and destroyed here; the strings are static and NUL-ended.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let built = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Show Lobby")).and(AppendMenuW(
            menu,
            MF_STRING,
            MENU_QUIT,
            w!("Quit"),
        ));
        let chosen = if built.is_ok() {
            let _ = SetForegroundWindow(hwnd);
            let id = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                x,
                y,
                None,
                hwnd,
                None,
            );
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            id.0 as usize
        } else {
            0
        };
        let _ = DestroyMenu(menu);
        match chosen {
            MENU_SHOW => Some(HostEvent::Show),
            MENU_QUIT => Some(HostEvent::Quit),
            _ => None,
        }
    }
}
