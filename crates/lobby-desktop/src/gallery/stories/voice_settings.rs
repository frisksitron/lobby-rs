use gpui_kit::{
    AnyElement, App, Context, IntoElement as _, ParentElement as _, Styled as _, Window, div, px,
};

use lobby_client::{
    Compressor, CompressorAmount, GainUnavailable, MicGain, MicQuality, MicReading, Settings,
};

use crate::components::Meter;
use crate::gallery::{Gallery, Section};
use crate::kit::ActiveTheme as _;
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

fn with(change: impl FnOnce(&mut Settings)) -> Settings {
    let mut settings = Settings::default();
    change(&mut settings);
    settings
}

/// Every cleaning step on, with the compressor at its default amount.
fn cleaning() -> Settings {
    with(|s| {
        s.echo_cancellation = true;
        s.compressor = Compressor {
            on: true,
            amount: CompressorAmount::DEFAULT,
        };
    })
}

fn bare() -> Settings {
    with(|s| {
        s.echo_cancellation = false;
        s.noise_suppression.on = false;
        s.compressor.on = false;
    })
}

pub(super) fn voice_section(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let [input, output, ..] = &g.held.devices;
    let [steps_on, steps_off] = &g.held.voice_controls;
    let voice = |meter, settings: &Settings| {
        let controls = if settings.noise_suppression.on {
            steps_on
        } else {
            steps_off
        };
        VoiceSection::new(input, output, controls, meter, settings, |_, _, _| {})
            .gain(MicGain::Level(0.72))
    };
    let heard = || Meter::Level(TALKING);
    vec![
        Section::new(
            "voice-section",
            "Default window: noise is an Off, Gentle, Standard or Strong select and the compressor an Off, Gentle or Strong one, beside their lines",
            framed(ROOMY_W, voice(heard(), &cleaning()), cx),
        ),
        Section::new(
            "voice-section-small",
            "Small window: the selects stay beside their lines",
            framed(SMALL_W, voice(heard(), &cleaning()), cx),
        ),
        Section::new(
            "voice-section-bare",
            "The three steps off, the voice going out untouched: both selects say Off",
            framed(SMALL_W, voice(heard(), &bare()), cx),
        ),
        Section::new(
            "voice-section-monitor",
            "Hearing yourself: the button is pressed and the hint says to use headphones",
            framed(ROOMY_W, voice(heard(), &cleaning()).monitor(true), cx),
        ),
        Section::new(
            "voice-section-gain-reading",
            "Windows has not said the volume yet",
            framed(
                ROOMY_W,
                voice(heard(), &cleaning()).gain(MicGain::Reading),
                cx,
            ),
        ),
        Section::new(
            "voice-section-gain-unavailable",
            "The microphone's volume cannot be changed: the reason takes the slider's place",
            framed(
                SMALL_W,
                voice(heard(), &cleaning()).gain(MicGain::Unavailable(GainUnavailable::NoControl)),
                cx,
            ),
        ),
        Section::new(
            "voice-section-silent",
            "Silence",
            framed(
                ROOMY_W,
                voice(Meter::Level(MicReading::SILENT), &cleaning()),
                cx,
            ),
        ),
        Section::new(
            "voice-section-quiet",
            "Small window, too quiet: the hint takes the line that was kept for it",
            framed(SMALL_W, voice(Meter::Level(QUIET), &cleaning()), cx),
        ),
        Section::new(
            "voice-section-clipping",
            "Clipping: the top segments turn coral",
            framed(ROOMY_W, voice(Meter::Level(CLIPPING), &cleaning()), cx),
        ),
        Section::new(
            "voice-section-failed",
            "The microphone cannot be listened to: the reason takes the hint's line",
            framed(
                ROOMY_W,
                voice(
                    Meter::Failed("The audio device is in use by another application.".into()),
                    &cleaning(),
                ),
                cx,
            ),
        ),
    ]
}
