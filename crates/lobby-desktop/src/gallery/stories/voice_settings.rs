use gpui_kit::{
    AnyElement, App, Context, IntoElement as _, ParentElement as _, Styled as _, Window,
    component::ActiveTheme as _, div, px,
};

use lobby_client::{DspReading, MicGain, MicQuality, MicReading, Settings};

use crate::components::{Meter, RackLayout, RackModel};
use crate::gallery::{Gallery, Section};
use crate::voice_settings::VoiceSection;

/// The small window's column, and the default window's.
const SMALL_W: f32 = 448.;
const ROOMY_W: f32 = 488.;

const fn reading(level_db: f32, peak_db: f32, quality: MicQuality) -> MicReading {
    MicReading {
        level_db,
        peak_db,
        quality,
    }
}

const TALKING: MicReading = reading(-24., -13., MicQuality::Good);
const QUIET: MicReading = reading(-54., -47., MicQuality::TooQuiet);
const CLIPPING: MicReading = reading(-6., 0., MicQuality::Clipping);
const CALL: DspReading = DspReading {
    mic_db: -24.,
    after_echo_db: -24.,
    after_noise_db: -25.,
    out_db: -22.,
    reduction_db: 2.,
    limit_db: 0.,
};

fn framed(width: f32, body: impl gpui_kit::IntoElement, cx: &App) -> AnyElement {
    div()
        .w(px(width + 24.))
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .child(body)
        .into_any_element()
}

pub(super) fn voice_section(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let [input, output, ..] = &g.held.devices;
    let settings = Settings::default();
    let voice = |meter, levels, layout| {
        VoiceSection::new(
            input,
            output,
            meter,
            RackModel::new(&settings, levels),
            settings.sounds,
            |_, _, _| {},
        )
        .rack_layout(layout)
        .gain(MicGain::Level(0.72))
    };
    let wide = RackLayout::Wide;
    vec![
        Section::new(
            "voice-section",
            "Small window, microphone heard",
            framed(SMALL_W, voice(Meter::Level(TALKING), None, wide), cx),
        ),
        Section::new(
            "voice-section-call",
            "In a call: the meter is the call's own, and the rack shows its levels",
            framed(ROOMY_W, voice(Meter::Level(TALKING), Some(CALL), wide), cx),
        ),
        Section::new(
            "voice-section-monitor",
            "Small window, MONITOR on",
            framed(
                SMALL_W,
                voice(Meter::Level(TALKING), Some(CALL), wide).monitor(true),
                cx,
            ),
        ),
        Section::new(
            "voice-section-silent",
            "Silence",
            framed(
                ROOMY_W,
                voice(Meter::Level(MicReading::SILENT), None, wide),
                cx,
            ),
        ),
        Section::new(
            "voice-section-quiet",
            "Small window, too quiet: the hint takes the line that was reserved for it",
            framed(SMALL_W, voice(Meter::Level(QUIET), None, wide), cx),
        ),
        Section::new(
            "voice-section-clipping",
            "Clipping",
            framed(ROOMY_W, voice(Meter::Level(CLIPPING), None, wide), cx),
        ),
        Section::new(
            "voice-section-failed",
            "The microphone cannot be listened to",
            framed(
                ROOMY_W,
                voice(
                    Meter::Failed("The audio device is in use by another application.".into()),
                    None,
                    wide,
                ),
                cx,
            ),
        ),
    ]
}
