//! The desktop host's platform pieces: OS idle time, the OS window (placement, hiding, showing),
//! the Sharer's ink overlay, the icon in the notification area, the one-instance pipe, and the
//! app's own install.

#[cfg(windows)]
pub mod install;
pub mod single_instance;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod win;

use std::{ffi::OsString, rc::Rc, time::Duration};

use gpui_kit::App;
use lobby_client::DEEP_LINK_SCHEME;
use tokio::sync::mpsc::UnboundedSender;

/// What the OS asks of the app from outside its window: the tray's menu and click, and a second
/// launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    Show,
    Quit,
    /// Someone started Lobby again: show the one that runs.
    Relaunched,
    /// Setup or uninstall is about to replace or delete the app: quit, and leave a downloaded
    /// update where it is.
    QuitForSetup,
    /// A `lobby://` link was opened, as the OS gave it. The Shell decides what it means.
    OpenLink(String),
}

/// The `lobby://` link a launch was given, if any (`args` are the arguments after the exe's
/// name). Any argument that starts with the scheme counts, so a link cannot pass for a flag.
pub fn link_in(args: impl IntoIterator<Item = OsString>) -> Option<String> {
    let prefix = format!("{DEEP_LINK_SCHEME}:");
    args.into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .find(|arg| {
            arg.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
        })
}

/// What the Shell does to its OS window. The app's is [`os_window_host`]; tests hand in one that
/// counts, since the test platform has no OS window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCommand {
    Hide,
    /// Brings the window back and to the front.
    Show,
    /// Brings it back without taking the focus: a driven client must never take the person's.
    ShowQuietly,
}

pub type WindowHost = Rc<dyn Fn(Option<NativeWindow>, WindowCommand, &mut App)>;

pub fn os_window_host() -> WindowHost {
    Rc::new(|native, command, cx| {
        let Some(native) = native else { return };
        // Both calls send window messages synchronously, so not inside this update.
        cx.spawn(async move |_| native.command(command)).detach();
    })
}

/// The icon in the notification area. Dropping it removes the icon. Not `Send`: the OS window
/// behind it belongs to the thread that made it.
pub struct Tray {
    #[cfg(windows)]
    _icon: tray::TrayIcon,
}

impl Tray {
    /// `None` where there is no notification area to put it in (or this platform has no tray yet).
    /// With `shown` false the icon is registered but hidden: a driven client keeps the person's
    /// tray clear.
    pub fn add(tooltip: &str, shown: bool, events: UnboundedSender<HostEvent>) -> Option<Self> {
        #[cfg(windows)]
        {
            tray::TrayIcon::add(tooltip, shown, events).map(|icon| Self { _icon: icon })
        }
        #[cfg(not(windows))]
        {
            let _ = (tooltip, shown, events);
            None
        }
    }
}

/// The session's time without keyboard or mouse input. `None` where the platform cannot tell, so
/// the Member is never shown idle.
pub fn idle_time() -> Option<Duration> {
    #[cfg(windows)]
    {
        win::idle_time()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// What the Sharer's ink overlay covers, as the capture has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayTarget {
    /// A whole monitor, by its `HMONITOR`: the overlay is topmost over all of it.
    Monitor(isize),
    /// Another app's window, by its `HWND`: the overlay sits just above it and follows it.
    Window(isize),
}

/// Why the Sharer's ink overlay cannot keep ink out of the share: drawing is then unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OverlayFault {
    #[error("the overlay has no window of its own")]
    NoWindow,
    #[error("the overlay's window is not Per-Monitor V2 DPI aware")]
    Dpi,
    #[error("the overlay's window has no per-pixel alpha")]
    Alpha,
    #[error("Windows did not make the overlay click-through")]
    Styles,
    #[error("Windows did not keep the overlay out of captures")]
    Exclusion,
    #[error("the shared monitor or window is gone")]
    SourceGone,
    #[error("Windows does not say when the shared window moves")]
    Follow,
    #[cfg(not(windows))]
    #[error("there is no overlay on this platform")]
    Unsupported,
}

/// Runs `open` so that a window it creates is Per-Monitor V2 DPI aware, whatever the process is.
pub fn per_monitor_v2<T>(open: impl FnOnce() -> T) -> T {
    #[cfg(windows)]
    {
        win::per_monitor_v2(open)
    }
    #[cfg(not(windows))]
    {
        open()
    }
}

/// Follows another app's window: `moved` hears whenever it moves, changes size, is minimised or
/// restored, hidden or shown, cloaked or uncloaked, or brought forward. Made and dropped on the
/// thread whose message loop runs: GPUI's.
pub struct WindowWatch {
    #[cfg(windows)]
    _hooks: win::WindowWatch,
}

impl WindowWatch {
    pub fn start(window: isize, moved: UnboundedSender<()>) -> Result<Self, OverlayFault> {
        #[cfg(windows)]
        {
            win::WindowWatch::start(window, moved)
                .map(|hooks| Self { _hooks: hooks })
                .ok_or(OverlayFault::Follow)
        }
        #[cfg(not(windows))]
        {
            let _ = (window, moved);
            Err(OverlayFault::Unsupported)
        }
    }
}

/// Where a window stands, for the overlay check.
#[cfg(all(feature = "automation", windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    /// As captures see it, in physical pixels: x, y, width and height.
    pub frame: (i32, i32, i32, i32),
    pub shown: bool,
    pub topmost: bool,
    /// The window just above it in the z-order.
    pub above: Option<isize>,
}

/// What the overlay check does to the window it shares.
#[cfg(all(feature = "automation", windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveWindow {
    /// x, y, width and height of the whole window, in physical pixels.
    Place(i32, i32, i32, i32),
    Minimise,
    Restore,
    /// To the top of the z-order, without activating it: the overlay is left under it.
    Raise,
}

/// The OS window under a GPUI window. Its calls send window messages synchronously, so they are
/// made from a task, never inside a GPUI update: the messages must reach GPUI while the app is not
/// borrowed, or GPUI drops them with "RefCell already borrowed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeWindow(#[cfg_attr(not(windows), allow(dead_code))] isize);

impl NativeWindow {
    pub fn of(window: &gpui_kit::Window) -> Option<Self> {
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            // `Window::window_handle` (GPUI's own) shadows the trait's method.
            match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => Some(Self(h.hwnd.get())),
                _ => None,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = window;
            None
        }
    }

    fn command(self, command: WindowCommand) {
        #[cfg(windows)]
        match command {
            WindowCommand::Hide => tray::hide_window(self.0),
            WindowCommand::Show => tray::show_window(self.0, true),
            WindowCommand::ShowQuietly => tray::show_window(self.0, false),
        }
        #[cfg(not(windows))]
        let _ = command;
    }

    pub fn park_offscreen(self) -> bool {
        #[cfg(windows)]
        {
            win::park_offscreen(self.0)
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Makes this window the Sharer's ink overlay over `target`, hidden: clicks pass through it,
    /// it never takes the focus, and captures leave it out. `LOBBY_OVERLAY_FAULT=exclusion` in
    /// an automation build fails it as Windows refusing the exclusion would.
    pub fn make_overlay(self, target: OverlayTarget) -> Result<(), OverlayFault> {
        #[cfg(feature = "automation")]
        if std::env::var("LOBBY_OVERLAY_FAULT").as_deref() == Ok("exclusion") {
            return Err(OverlayFault::Exclusion);
        }
        #[cfg(windows)]
        {
            win::make_overlay(self.0, target)
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(OverlayFault::Unsupported)
        }
    }

    /// Shows the overlay over `target` without activating it, or hides it. Over a window it
    /// stays hidden while the window is minimised, cloaked or hidden. False when the target is
    /// gone.
    pub fn show_overlay(self, target: OverlayTarget, shown: bool) -> bool {
        #[cfg(windows)]
        {
            win::show_overlay(self.0, target, shown)
        }
        #[cfg(not(windows))]
        {
            let _ = (target, shown);
            false
        }
    }

    /// Keeps the window out of captures of the screen, or lets it back in. True when Windows
    /// reads back what was asked.
    pub fn exclude_from_capture(self, excluded: bool) -> bool {
        #[cfg(windows)]
        {
            win::exclude_from_capture(self.0, excluded)
        }
        #[cfg(not(windows))]
        {
            let _ = excluded;
            false
        }
    }

    /// Where the client area starts on the screen, in physical pixels.
    pub fn client_origin(self) -> Option<(i32, i32)> {
        #[cfg(windows)]
        {
            win::client_origin(self.0)
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Whether most of the window is on `monitor`.
    pub fn on_monitor(self, monitor: isize) -> bool {
        #[cfg(windows)]
        {
            win::on_monitor(self.0, monitor)
        }
        #[cfg(not(windows))]
        {
            let _ = monitor;
            false
        }
    }

    /// The window a click at `x`, `y` (physical pixels) would land on, for the overlay check.
    #[cfg(all(feature = "automation", windows))]
    pub fn at(x: i32, y: i32) -> Option<Self> {
        win::window_at(x, y).map(Self)
    }

    /// The window with the keyboard focus, of any process, for the overlay check.
    #[cfg(all(feature = "automation", windows))]
    pub fn foreground() -> Option<Self> {
        win::foreground().map(Self)
    }

    /// Another process's window by its `HWND`, for the overlay check.
    #[cfg(all(feature = "automation", windows))]
    pub fn from_raw(hwnd: isize) -> Self {
        Self(hwnd)
    }

    #[cfg(all(feature = "automation", windows))]
    pub fn raw(self) -> isize {
        self.0
    }

    /// Where the window stands, for the overlay check; `None` once it is gone.
    #[cfg(all(feature = "automation", windows))]
    pub fn state(self) -> Option<WindowState> {
        win::window_state(self.0)
    }

    /// Moves, minimises or restores the window without activating it, for the overlay check.
    #[cfg(all(feature = "automation", windows))]
    pub fn drive(self, to: DriveWindow) -> bool {
        win::drive_window(self.0, to)
    }

    /// Gives the window a client area of `width`x`height` physical pixels without activating it
    /// (GPUI's own resize activates the window).
    pub fn resize_client(self, width: i32, height: i32) -> bool {
        #[cfg(windows)]
        {
            win::resize_client(self.0, width, height)
        }
        #[cfg(not(windows))]
        {
            let _ = (width, height);
            false
        }
    }
}
