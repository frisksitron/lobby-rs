use gpui_kit::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement as _, Styled as _, Window,
    component::{ActiveTheme as _, select::SelectState},
    div, px,
};
use lobby_client::{
    Compressor, CompressorAmount, DspReading, GainUnavailable, MicGain, MicQuality, MicReading,
    NoiseSuppression, Settings,
};

use crate::components::{Meter, PatchRack, RackLayout, RackModel};
use crate::gallery::{Gallery, Section};
use crate::voice_settings::{Items, picker};

/// The Settings column at the default window and at the small one, and at the small one at 150 %
/// (in design pixels), where the rack stacks.
const WIDE_W: f32 = 488.;
const SMALL_W: f32 = 448.;
const NARROW_W: f32 = 288.;

const fn reading(mic: f32, echo: f32, noise: f32, out: f32, reduction: f32) -> DspReading {
    DspReading {
        mic_db: mic,
        after_echo_db: echo,
        after_noise_db: noise,
        out_db: out,
        reduction_db: reduction,
        limit_db: 0.,
    }
}

const TALKING: DspReading = reading(-22., -22., -23., -20., 2.);
const LOUD: DspReading = DspReading {
    limit_db: 2.5,
    ..reading(-6., -6., -7., -9., 10.)
};
const NOISY_ROOM: DspReading = reading(-30., -30., -48., -46., 0.);
const ECHOING: DspReading = reading(-24., -40., -41., -38., 0.);
/// One word, from its first sound to its loudest.
const WORD: [DspReading; 4] = [
    reading(-50., -50., -52., -50., 0.),
    reading(-38., -38., -40., -36., 0.),
    reading(-26., -26., -27., -24., 2.),
    reading(-14., -14., -15., -14., 6.),
];
const MIC_TALKING: MicReading = MicReading {
    level_db: -22.,
    peak_db: -12.,
    quality: MicQuality::Good,
};
/// Windows' input level as most microphones ship.
const MIC_GAIN: f32 = 0.72;

fn rack(settings: Settings, levels: Option<DspReading>) -> RackModel {
    RackModel::new(&settings, levels)
}

fn with(change: impl FnOnce(&mut Settings)) -> Settings {
    let mut settings = Settings::default();
    change(&mut settings);
    settings
}

fn heavy() -> Settings {
    with(|s| {
        s.echo_cancellation = true;
        s.compressor = Compressor {
            on: true,
            amount: CompressorAmount::new(85),
        };
    })
}

fn noise_out() -> Settings {
    with(|s| {
        s.noise_suppression = NoiseSuppression {
            on: false,
            ..NoiseSuppression::default()
        }
    })
}

fn framed(width: f32, body: impl IntoElement, cx: &App) -> AnyElement {
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

type Selects<'a> = (
    &'a Entity<SelectState<Items>>,
    &'a Entity<SelectState<Items>>,
);

fn patched(model: RackModel, layout: RackLayout, (input, output): Selects) -> PatchRack {
    PatchRack::new(model, layout)
        .devices(
            picker(input, "input-device", lobby_client::copy::DEVICE_INPUT),
            picker(output, "output-device", lobby_client::copy::DEVICE_OUTPUT),
        )
        .meter(Meter::Level(if model.live() {
            MIC_TALKING
        } else {
            MicReading::SILENT
        }))
        .gain(MicGain::Level(MIC_GAIN), |_, _, _| {})
}

fn monitoring(rack: PatchRack) -> PatchRack {
    rack.monitor(true, |_, _, _| {})
}

fn swings(
    layout: RackLayout,
    width: f32,
    from: RackModel,
    to: RackModel,
    selects: Selects,
    cx: &App,
) -> AnyElement {
    super::wrap([0.2, 0.45, 0.7].map(|t| {
        super::caption(
            format!("{:.0} % of the swing", t * 100.),
            framed(width, patched(to, layout, selects).swing(from, t), cx),
            cx,
        )
    }))
    .into_any_element()
}

pub(super) fn patch_rack(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let [input, output, long_input, long_output] = &g.held.devices;
    let sample = (input, output);
    let long = (long_input, long_output);
    let wide = |model| framed(WIDE_W, patched(model, RackLayout::Wide, sample), cx);
    let small = |model| framed(SMALL_W, patched(model, RackLayout::Wide, sample), cx);
    let narrow = |model| framed(NARROW_W, patched(model, RackLayout::Stacked, sample), cx);
    let wide_with = |rack| framed(WIDE_W, rack, cx);
    let small_with = |rack| framed(SMALL_W, rack, cx);
    let narrow_with = |rack| framed(NARROW_W, rack, cx);
    let default = Settings::default();
    let echo_out = with(|s| s.echo_cancellation = false);
    let echo_in = with(|s| s.echo_cancellation = true);
    let comp_out = with(|s| s.compressor.on = false);
    let direct = with(|s| s.studio_mode = true);
    vec![
        Section::new(
            "rack-call",
            "In a call: every module in, the voice lit along its cables",
            wide(rack(echo_in.clone(), Some(TALKING))),
        ),
        Section::new(
            "rack-rest",
            "Outside a call: nothing goes through, so the cables and the faces rest",
            wide(rack(default.clone(), None)),
        ),
        Section::new(
            "rack-word",
            "One word as it gets louder: the ripples reach out, the bars rise, the cone breathes",
            super::wrap(WORD.map(|r| wide(rack(echo_in.clone(), Some(r))))),
        ),
        Section::new(
            "rack-echo-at-work",
            "ECHO taking echo out: the ripples draw in and die sooner",
            super::wrap([
                wide(rack(echo_in.clone(), Some(TALKING))),
                wide(rack(echo_in.clone(), Some(ECHOING))),
            ]),
        ),
        Section::new(
            "rack-echo-out",
            "ECHO taken out: still and dim, its cable swings around it",
            wide(rack(echo_out, Some(TALKING))),
        ),
        Section::new(
            "rack-noise-at-work",
            "NOISE at work in a noisy room: grain over quiet bars",
            wide(rack(default.clone(), Some(NOISY_ROOM))),
        ),
        Section::new(
            "rack-noise-out",
            "NOISE taken out: calm, no grain, its engine kept",
            wide(rack(noise_out(), Some(NOISY_ROOM))),
        ),
        Section::new(
            "rack-compressing",
            "COMP working hard on a loud voice: the needle swings left, the limiter's light is on",
            wide(rack(heavy(), Some(LOUD))),
        ),
        Section::new(
            "rack-comp-out",
            "COMP taken out: the needle rests",
            wide(rack(comp_out, Some(LOUD))),
        ),
        Section::new(
            "rack-direct",
            "Direct: one cable from MIC to OUT around the whole rack, the limiter dark",
            wide(rack(direct.clone(), Some(LOUD))),
        ),
        Section::new(
            "rack-long-names",
            "Long device names: cut off with an ellipsis, whole in a tooltip and in the list",
            framed(
                WIDE_W,
                patched(rack(default.clone(), None), RackLayout::Wide, long),
                cx,
            ),
        ),
        Section::new(
            "rack-gain",
            "GAIN, the microphone's Windows input level: off, a third, as most ship, full",
            super::wrap([0., 0.33, MIC_GAIN, 1.].map(|level| {
                small_with(
                    patched(rack(default.clone(), None), RackLayout::Wide, sample)
                        .gain(MicGain::Level(level), |_, _, _| {}),
                )
            })),
        ),
        Section::new(
            "rack-gain-unavailable",
            "GAIN with nothing to set: at rest and set aside, its tooltip says why",
            super::wrap(
                [
                    MicGain::Reading,
                    MicGain::Unavailable(GainUnavailable::NoDevice),
                    MicGain::Unavailable(GainUnavailable::NoControl),
                ]
                .map(|gain| {
                    small_with(
                        patched(rack(default.clone(), None), RackLayout::Wide, sample)
                            .gain(gain, |_, _, _| {}),
                    )
                }),
            ),
        ),
        Section::new(
            "rack-monitor",
            "MONITOR on, in a call or out of one: the voice goes through, and OUT asks for \
             headphones under its select",
            super::wrap([
                wide_with(monitoring(patched(
                    rack(echo_in.clone(), Some(TALKING)),
                    RackLayout::Wide,
                    sample,
                ))),
                small_with(monitoring(patched(
                    rack(default.clone(), Some(TALKING)),
                    RackLayout::Wide,
                    sample,
                ))),
                narrow_with(monitoring(patched(
                    rack(echo_in.clone(), Some(TALKING)),
                    RackLayout::Stacked,
                    sample,
                ))),
            ]),
        ),
        Section::new(
            "rack-mic-failed",
            "The microphone cannot be listened to: the reason takes the meter's room",
            framed(
                WIDE_W,
                patched(rack(default.clone(), None), RackLayout::Wide, sample).meter(
                    Meter::Failed("The audio device is in use by another application.".into()),
                ),
                cx,
            ),
        ),
        Section::new(
            "rack-swing-noise",
            "Taking NOISE out, mid-swing",
            swings(
                RackLayout::Wide,
                WIDE_W,
                rack(default.clone(), Some(TALKING)),
                rack(noise_out(), Some(TALKING)),
                sample,
                cx,
            ),
        ),
        Section::new(
            "rack-swing-direct",
            "Going Direct, mid-swing",
            swings(
                RackLayout::Wide,
                WIDE_W,
                rack(default.clone(), Some(TALKING)),
                rack(direct.clone(), Some(TALKING)),
                sample,
                cx,
            ),
        ),
        Section::new(
            "rack-small",
            "Small window: the same rack in the narrower column",
            super::wrap([
                small(rack(echo_in.clone(), Some(TALKING))),
                small(rack(default.clone(), None)),
            ]),
        ),
        Section::new(
            "rack-narrow",
            "Small window at a large UI scale: stacked, the cables down the side",
            super::wrap([
                narrow(rack(echo_in.clone(), Some(TALKING))),
                narrow(rack(noise_out(), Some(NOISY_ROOM))),
                narrow(rack(direct.clone(), Some(TALKING))),
            ]),
        ),
        Section::new(
            "rack-narrow-swing",
            "Stacked, taking NOISE out, mid-swing",
            swings(
                RackLayout::Stacked,
                NARROW_W,
                rack(default.clone(), Some(TALKING)),
                rack(noise_out(), Some(TALKING)),
                sample,
                cx,
            ),
        ),
    ]
}
