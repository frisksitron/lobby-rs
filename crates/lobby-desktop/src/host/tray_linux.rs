//! The Linux desktop's notification-area icon: a StatusNotifierItem. Show and Quit are host events;
//! what Show does to the window is the Shell's (`shell/reopen.rs`).

use image::ImageFormat;
use ksni::{
    Icon, MenuItem, Status, ToolTip, Tray,
    blocking::{Handle, TrayMethods as _},
    menu::StandardItem,
};
use std::thread;

use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tracing::{info, warn};

use super::HostEvent;
use crate::host_env::{APP_ID, APP_NAME};

/// The sizes panels draw a tray icon at; the notification area picks the nearest. Each is a
/// frame of `packaging/windows/lobby.ico` drawn at that size (22 is its 24 frame scaled down),
/// never a bigger one scaled to fit, so the glyph stays sharp.
const TRAY_ICONS: [&[u8]; 4] = [
    include_bytes!("../../../../packaging/linux/icons/lobby-22.png"),
    include_bytes!("../../../../packaging/linux/icons/lobby-24.png"),
    include_bytes!("../../../../packaging/linux/icons/lobby-32.png"),
    include_bytes!("../../../../packaging/linux/icons/lobby-48.png"),
];

struct LobbyTray {
    tooltip: String,
    shown: bool,
    icons: Vec<Icon>,
    events: UnboundedSender<HostEvent>,
}

impl LobbyTray {
    fn send(&self, event: HostEvent) {
        let _ = self.events.send(event);
    }
}

impl Tray for LobbyTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        self.tooltip.clone()
    }

    /// Passive items are the ones a panel may leave out: a driven client keeps the tray clear.
    fn status(&self) -> Status {
        if self.shown {
            Status::Active
        } else {
            Status::Passive
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: self.tooltip.clone(),
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(HostEvent::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: format!("Show {APP_NAME}"),
                activate: Box::new(|tray: &mut Self| tray.send(HostEvent::Show)),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.send(HostEvent::Quit)),
                ..StandardItem::default()
            }
            .into(),
        ]
    }
}

/// The icon in the notification area; dropping it asks for the icon to be taken away and does not
/// wait: the UI thread must not stand on a bus that is stalled. At exit the bus drops the item
/// with the connection anyway.
pub(super) struct TrayIcon {
    handle: Handle<LobbyTray>,
}

impl TrayIcon {
    /// Registers on a thread of its own, since the bus may be stalled and registering has no
    /// time limit of its own. The answer is `None` when no StatusNotifierWatcher is on the bus
    /// (GNOME without its AppIndicator extension, a bare compositor): there is then no icon to
    /// bring the window back. Whoever waits for the answer sets its own limit; an icon that
    /// registers after the receiver is gone is dropped, and so removed.
    pub(super) fn register(
        tooltip: &str,
        shown: bool,
        events: UnboundedSender<HostEvent>,
    ) -> oneshot::Receiver<Option<super::Tray>> {
        let (answer, answered) = oneshot::channel();
        let tray = LobbyTray {
            tooltip: tooltip.to_owned(),
            shown,
            icons: icons(),
            events,
        };
        let spawned = thread::Builder::new()
            .name("tray-register".into())
            .spawn(move || {
                let icon = match tray.spawn() {
                    Ok(handle) => {
                        info!(shown, "tray icon registered");
                        Some(super::Tray {
                            _icon: Self { handle },
                        })
                    }
                    Err(e) => {
                        warn!(error = %e, "cannot register the tray icon");
                        None
                    }
                };
                let _ = answer.send(icon);
            });
        if let Err(error) = spawned {
            warn!(%error, "no thread to register the tray icon");
        }
        answered
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        let _ = self.handle.shutdown();
    }
}

/// The tray icons as the ARGB32 pixmaps a StatusNotifierItem carries (network byte order).
fn icons() -> Vec<Icon> {
    TRAY_ICONS
        .iter()
        .filter_map(|png| image::load_from_memory_with_format(png, ImageFormat::Png).ok())
        .map(|png| {
            let rgba = png.into_rgba8();
            Icon {
                width: rgba.width() as i32,
                height: rgba.height() as i32,
                data: rgba
                    .pixels()
                    .flat_map(|p| [p[3], p[0], p[1], p[2]])
                    .collect(),
            }
        })
        .collect()
}
