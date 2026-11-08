//! Icons the app embeds: the kit's defaults plus the extra Lucide icons it uses, one by one.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString, assets::Assets};

mod extra {
    gpui_kit::assets::icon_assets!(
        pub Icons,
        [
            Activity,
            AppWindow,
            AudioLines,
            BellOff,
            Bold,
            ChevronsUpDown,
            CircleAlert,
            Clock,
            Code,
            Download,
            FileArchive,
            FileImage,
            FileMusic,
            FilePlay,
            CircleMinus,
            CircleCheck,
            CircleDot,
            CloudOff,
            Flag,
            FlaskConical,
            FolderOpen,
            Globe,
            HardDrive,
            HeadphoneOff,
            Headphones,
            Hourglass,
            ImageOff,
            Italic,
            KeyRound,
            Link,
            List,
            ListOrdered,
            Loader,
            Lock,
            LoaderCircle,
            LogOut,
            Mail,
            MessageCircle,
            Mic,
            MicOff,
            Minus,
            Monitor,
            MonitorSmartphone,
            Paperclip,
            Pencil,
            Phone,
            Plus,
            PhoneOff,
            Plug,
            RefreshCw,
            RotateCcw,
            RotateCw,
            ScreenShare,
            ScreenShareOff,
            SendHorizontal,
            SquareCode,
            Strikethrough,
            TextQuote,
            ServerCrash,
            TriangleAlert,
            Type,
            Unplug,
            UserCheck,
            UserMinus,
            UserPen,
            UserPlus,
            UserX,
            Users,
            Wifi,
            WifiOff,
            X
        ]
    );
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LobbyAssets;

impl AssetSource for LobbyAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match extra::Icons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = extra::Icons.list(path)?;
        names.extend(Assets.list(path)?);
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_the_lobby_view_uses_loads() {
        for path in [
            "icons/mic-off.svg",
            "icons/minus.svg",
            "icons/rotate-ccw.svg",
            "icons/loader-circle.svg",
            "icons/phone-off.svg",
            "icons/screen-share.svg",
            "icons/monitor.svg",
            "icons/app-window.svg",
            "icons/check.svg",
            "icons/cloud-off.svg",
            "icons/wifi-off.svg",
            "icons/server-crash.svg",
            "icons/circle-check.svg",
            "icons/plug.svg",
            "icons/rotate-cw.svg",
            "icons/monitor-smartphone.svg",
            "icons/triangle-alert.svg",
            "icons/unplug.svg",
            "icons/message-circle.svg",
            "icons/bold.svg",
            "icons/italic.svg",
            "icons/strikethrough.svg",
            "icons/code.svg",
            "icons/link.svg",
            "icons/list.svg",
            "icons/list-ordered.svg",
            "icons/text-quote.svg",
            "icons/square-code.svg",
            "icons/flag.svg",
            "icons/arrow-down.svg",
            "icons/arrow-up.svg",
            "icons/circle-dot.svg",
            "icons/chevrons-up-down.svg",
            "icons/bell.svg",
            "icons/bell-off.svg",
            "icons/wifi.svg",
            "icons/users.svg",
            "icons/moon.svg",
            "icons/circle-minus.svg",
            "icons/window-close.svg",
            "icons/window-minimize.svg",
            "icons/window-maximize.svg",
            "icons/window-restore.svg",
            "icons/headphones.svg",
            "icons/globe.svg",
            "icons/circle-alert.svg",
            "icons/audio-lines.svg",
            "icons/plus.svg",
            "icons/hard-drive.svg",
            "icons/activity.svg",
            "icons/pencil.svg",
            "icons/user-pen.svg",
        ] {
            let svg = LobbyAssets.load(path).unwrap().expect(path);
            // Some SVGs start with an XML prolog.
            assert!(svg.windows(4).any(|w| w == b"<svg"), "{path}");
        }
    }
}
