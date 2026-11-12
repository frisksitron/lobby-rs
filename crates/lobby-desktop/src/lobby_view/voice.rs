//! Voice from the Lobby view: which devices a call uses, join, leave, mute and deafen, the Voice
//! stats dialog, and the strip that docks above the composer when the sidebar is folded.

use std::sync::Arc;

use gpui_kit::{AppContext as _, Context, ParentElement as _, Styled as _, Window, div};
use lobby_client::{
    DeviceSource, DspControl, FakeDevices, Intent, IssueKind, LobbyState, MediaConfig, MicLevel,
    MonitorControl, Settings, SystemDevices,
};

use super::{LobbyView, forward};
use crate::components::{
    TilePress, VoiceFace, VoiceStatsDialog, VoiceStrip, Waits, card_surface, pictures,
    stats_dialog_width, voice_bar_model,
};
use crate::kit::{WindowExt as _, dialog};
use crate::scale::{design_viewport, device, u};
use crate::voice_shared::VoiceShared;

/// The air between the feed and the folded window's Voice bar.
const STRIP_TOP: f32 = 4.;

/// Which devices Voice uses: the ones Settings chose on this computer, or `LOBBY_FAKE_MEDIA=1`'s
/// tone, no speakers and a fixed list of pretend devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceDevices {
    System,
    Fake,
}

impl VoiceDevices {
    /// Read at every Join, so a device chosen in Settings (or plugged in since) is the one used,
    /// and by Settings' Monitor with no call on. Both kinds clean the microphone as `dsp` says.
    pub(crate) fn config(
        self,
        settings: &Settings,
        level: MicLevel,
        dsp: DspControl,
        monitor: MonitorControl,
    ) -> MediaConfig {
        match self {
            Self::System => MediaConfig {
                monitor,
                ..MediaConfig::devices(
                    settings.input_device.clone(),
                    settings.output_device.clone(),
                    self.source(),
                    level,
                    dsp,
                )
            },
            Self::Fake => MediaConfig {
                dsp,
                monitor,
                ..MediaConfig::fake()
            },
        }
    }

    /// Where the Settings page and a call ask which devices exist.
    pub fn source(self) -> Arc<dyn DeviceSource> {
        match self {
            Self::System => Arc::new(SystemDevices),
            Self::Fake => Arc::new(FakeDevices::new(FakeDevices::sample())),
        }
    }

    /// `LOBBY_FAKE_MEDIA=1` adds the synthetic test pattern to the source picker.
    pub(super) fn test_patterns(self) -> bool {
        self == Self::Fake
    }
}

impl LobbyView {
    pub fn join_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_rejoins(window, cx);
        let VoiceShared {
            level,
            dsp,
            monitor,
        } = VoiceShared::of(cx);
        #[cfg_attr(not(feature = "automation"), allow(unused_mut))]
        let mut media = self
            .devices
            .config(self.lobbies.read(cx).settings(), level, dsp, monitor);
        #[cfg(feature = "automation")]
        crate::automation::stage_faults(&mut media, cx);
        let volumes = self
            .session
            .read(cx)
            .origin()
            .map(|origin| self.lobbies.read(cx).settings().volumes_of(origin))
            .unwrap_or_default();
        // A refusal is the Session's Issue: it reaches the Center through `on_effects`.
        let join = Intent::JoinVoice {
            media: Box::new(media),
            volumes,
        };
        self.session.update(cx, |s, cx| s.dispatch(join, cx));
        cx.notify();
    }

    pub fn leave_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::LeaveVoice, cx));
        self.dismiss(IssueKind::VoiceRefused, window, cx);
        cx.notify();
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::ToggleMute, cx));
        cx.notify();
    }

    pub fn toggle_deafen(&mut self, cx: &mut Context<Self>) {
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::ToggleDeafen, cx));
        cx.notify();
    }

    pub fn open_voice_stats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice_stats.is_some() {
            return;
        }
        let lobby = cx.weak_entity();
        let body = cx.new(|cx| VoiceStatsDialog::new(self.session.clone(), cx));
        let view = body.clone();
        dialog::open(window, cx, move |dialog, window, _| {
            let lobby = lobby.clone();
            dialog
                .title("Voice stats")
                .w(device(
                    stats_dialog_width(design_viewport(window).0),
                    window,
                ))
                .child(body.clone())
                .on_close(move |_, _, cx| {
                    let _ = lobby.update(cx, |v, cx| {
                        v.voice_stats = None;
                        cx.notify();
                    });
                })
        });
        self.voice_stats = Some(view);
        cx.notify();
    }

    pub(super) fn close_voice_stats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice_stats.take().is_some() {
            window.close_dialog(cx);
            cx.notify();
        }
    }

    /// Folded, Voice's controls dock above the composer beside the faces of who is in it, in the
    /// order of the member sheet's Voice box. A sharer's face watches them as their tile does, and
    /// a right-click on any face opens that Member's card.
    pub(super) fn voice_strip(&self, state: &LobbyState, cx: &Context<Self>) -> gpui_kit::Div {
        let sidebar = self.sidebar.read(cx);
        let (groups, still) = (sidebar.groups(state), sidebar.rings_still());
        let surface = card_surface(cx);
        let press = forward(cx, |v: &mut Self, press, window, cx| match press {
            TilePress::Show(sharer) => v.show(sharer, window, cx),
            TilePress::Stop => v.close_viewer(window, cx),
        });
        let card = forward(cx, |v: &mut Self, (user, at), window, cx| {
            v.sidebar
                .update(cx, |s, cx| s.open_member_panel(&user, at, window, cx));
        });
        let faces = groups
            .in_voice
            .into_iter()
            .map(|row| {
                VoiceFace::new(row, groups.known, surface)
                    .pictures(pictures(self.images.clone()))
                    .on_press(press.clone())
                    .still(still)
                    .on_context(card.clone())
            })
            .collect();
        let now = cx.background_executor().now();
        div().px_4().pt(u(STRIP_TOP)).child(
            VoiceStrip::new(voice_bar_model(state), self.voice.clone())
                .waits(Waits::of(state, now))
                .faces(faces),
        )
    }
}
