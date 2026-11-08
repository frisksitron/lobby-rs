//! The audio handles a call and the Settings page share: the microphone's meter, how the
//! microphone is cleaned, and Monitor. They live here, apart from both, so the page never drives
//! the call: the Shell keeps the cleaning equal to the saved Settings ([`follow_settings`]), and
//! a call reads it from its next frame whether the page is open or not.

use gpui_kit::{App, Global};
use lobby_client::{DspControl, MicLevel, MonitorControl, Settings};

#[derive(Clone, Default)]
pub struct VoiceShared {
    /// The one meter of the window, so the call and the page see the same microphone.
    pub level: MicLevel,
    pub dsp: DspControl,
    /// Whether the person hears themselves. Never saved, and only the Settings page turns it on:
    /// it goes off when the page closes.
    pub monitor: MonitorControl,
}

impl Global for VoiceShared {}

impl VoiceShared {
    pub fn of(cx: &mut App) -> Self {
        cx.default_global::<Self>().clone()
    }
}

/// The one place the cleaning changes. The Shell calls it at startup and on every change of the
/// Settings.
pub fn follow_settings(settings: &Settings, cx: &mut App) {
    VoiceShared::of(cx).dsp.set(settings.into());
}
