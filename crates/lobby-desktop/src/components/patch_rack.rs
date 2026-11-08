//! The Voice rack: the microphone's path as panel modules patched with cables, MIC, ECHO, NOISE,
//! COMP, OUT. The order is fixed, since echo cancellation must hear the microphone as captured;
//! patching takes a module in or out of the path, and the cable swings around one taken out.
//! Direct is one cable from MIC to OUT around the whole rack: Studio Mode, past the limiter too.
//! MIC holds the microphone's picker, meter and GAIN, the microphone's own Windows level; OUT the
//! speakers' picker, MONITOR and the limiter's light.
//!
//! While the voice goes through, in a call or with MONITOR on, the cable carrying it is lavender,
//! as bright as the voice in it, and each module's face shows what it does to the voice
//! (`rack_faces`). Otherwise the cables rest warm grey and the faces are still. Wide, the three
//! processors stand side by side over MIC and OUT and the cables hang in the channel between the
//! rows; stacked for a narrow window, each module is a row and the cables loop down the left side.
//! The modules keep their places in every patch: only the cables move.

use std::{rc::Rc, time::Duration};

use gpui_kit::{
    AnyElement, App, Bounds, Div, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, RenderOnce, SharedString, Stateful, Styled as _,
    TestSupportExt as _, Window,
    base::{Spring, spring},
    canvas,
    component::{ActiveTheme as _, Colorize as _, Disableable as _, Sizable as _, switch::Switch},
    div,
    prelude::FluentBuilder as _,
};
use lobby_client::{
    Compressor, CompressorAmount, DspReading, MicGain, NoiseEngine, NoiseSuppression, Settings,
    copy,
};

use super::{
    Callback, Meter, MicMeterBar, Segmented,
    rack_faces::{self, Grain, Inks, Pen, Ripples, circle, stroke},
    rack_knob::{self, Dial, KNOB_MAX},
};
use crate::scale::{design_viewport, u, window_scale};

/// From this window width (design pixels) the rack is wide: below it, NOISE's engine choice and
/// OUT's picker beside its cone no longer fit side by side.
const WIDE_FROM: f32 = 420.;

const GAP: f32 = 4.;
const PANEL_RADIUS: f32 = 8.;
const PANEL_PAD: f32 = 8.;
const HEADER_H: f32 = 16.;
const FACE_GAP: f32 = 6.;
/// How far a module out of the path, and everything on its face, fades back.
const SET_ASIDE: f32 = 0.45;

const JACK_R: f32 = 7.;
const JACK_HOLE_R: f32 = 3.;
const PLUG_R: f32 = 6.;
const PLUG_CORE_R: f32 = 4.5;
/// A jack's centre from the edges of its module it sits nearest.
const JACK_INSET: f32 = 16.;
/// The strip a jack takes along its module's edge, inside the padding.
const JACK_BAND: f32 = 2. * JACK_INSET - PANEL_PAD;
const CABLE_W: f32 = 4.;
/// The dark edge either side of a cable, so it reads over a panel.
const CABLE_RIM: f32 = 1.5;
const GLOW_W: f32 = 12.;
const GLOW_ALPHA: f32 = 0.24;

/// The wide rack: the processors' row, the channel the cables hang in, and MIC and OUT's row.
const TOP_H: f32 = 128.;
const CHANNEL_H: f32 = 30.;
const IO_H: f32 = 104.;
/// How the processors share their row: ECHO, NOISE, COMP.
const TOP_SHARES: [f32; 3] = [5., 7., 7.];
/// The stacked rack: each module a row, in order, the cables in a gutter on the left.
const STACK_H: [f32; 5] = [104., 76., 76., 84., 84.];
const GUTTER: f32 = 24.;
/// A cable hangs this far past the straight line between its plugs, more for a longer one.
const SAG_BASE: f32 = 6.;
const SAG_PER_SPAN: f32 = 0.25;
const SAG_MAX: f32 = 40.;
const BULGE_PER_SPAN: f32 = 0.14;
const BULGE_MAX: f32 = 30.;
/// A cable from a row to the other bows out this far, away from the middle of the rack.
const BOW: f32 = 14.;

/// A cable's plug travels to its new jack in this time, and its slack settles a little after,
/// with a small swing.
const SWING: Spring = Spring::new(Duration::from_millis(260));
const SLACK: Spring = Spring::new(Duration::from_millis(440)).with_damping(0.6);
const UNPLUG: Spring = Spring::new(Duration::from_millis(200));

/// A cable is dim lavender at this level and full lavender from the next.
const GLOW_FLOOR_DB: f32 = -60.;
const GLOW_FULL_DB: f32 = -12.;
/// Levels are drawn in this many steps, so a steady level does not redraw.
const GLOW_STEPS: f32 = 16.;
/// How much ECHO and NOISE take out is drawn in steps up to this many dB.
const REMOVED_FULL_DB: f32 = 24.;
const REMOVED_STEPS: f32 = 8.;
/// COMP's needle reads up to this much reduction, in half-dB steps.
const REDUCTION_FULL_DB: f32 = 12.;
const REDUCTION_STEPS: f32 = 24.;
/// The limiter's light comes on past this much.
const LIMIT_LIT_DB: f32 = 0.5;

const OUT_CONE: f32 = 60.;
const STACK_CONE: f32 = 44.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RackLayout {
    Wide,
    Stacked,
}

impl RackLayout {
    pub fn for_window(window: &Window) -> Self {
        if design_viewport(window).0 >= WIDE_FROM {
            RackLayout::Wide
        } else {
            RackLayout::Stacked
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Module {
    Mic,
    Echo,
    Noise,
    Comp,
    Out,
}

const MODULES: [Module; 5] = [
    Module::Mic,
    Module::Echo,
    Module::Noise,
    Module::Comp,
    Module::Out,
];
/// The cables start at every module but OUT.
const SOURCES: usize = 4;

impl Module {
    fn name(self) -> &'static str {
        match self {
            Module::Mic => copy::RACK_MIC,
            Module::Echo => copy::RACK_ECHO,
            Module::Noise => copy::RACK_NOISE,
            Module::Comp => copy::RACK_COMP,
            Module::Out => copy::RACK_OUT,
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Module::Mic => "mic",
            Module::Echo => "echo",
            Module::Noise => "noise",
            Module::Comp => "comp",
            Module::Out => "out",
        }
    }

    fn has_in(self) -> bool {
        self != Module::Mic
    }

    fn has_out(self) -> bool {
        self != Module::Out
    }

    /// Whether the person can take it out of the path, and with what words.
    fn power(self) -> Option<(&'static str, &'static str)> {
        match self {
            Module::Echo => Some((copy::ECHO_CANCELLATION, copy::ECHO_CANCELLATION_DETAIL)),
            Module::Noise => Some((copy::NOISE_SUPPRESSION, copy::NOISE_SUPPRESSION_DETAIL)),
            Module::Comp => Some((copy::COMPRESSOR, copy::COMPRESSOR_DETAIL)),
            Module::Mic | Module::Out => None,
        }
    }
}

/// The level at each point of the chain, by the module whose out jack it leaves: MIC, ECHO,
/// NOISE, and COMP (whose out is what OUT sends).
fn carried(r: &DspReading) -> [f32; SOURCES] {
    [r.mic_db, r.after_echo_db, r.after_noise_db, r.out_db]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RackChange {
    Direct(bool),
    Echo(bool),
    Noise(bool),
    Engine(NoiseEngine),
    Compressor(bool),
    Amount(CompressorAmount),
}

impl RackChange {
    pub fn apply(self, s: &mut Settings) {
        match self {
            RackChange::Direct(on) => s.studio_mode = on,
            RackChange::Echo(on) => s.echo_cancellation = on,
            RackChange::Noise(on) => s.noise_suppression.on = on,
            RackChange::Engine(engine) => s.noise_suppression.engine = engine,
            RackChange::Compressor(on) => s.compressor.on = on,
            RackChange::Amount(amount) => s.compressor.amount = amount,
        }
    }
}

/// How the voice's levels look on the rack, in the steps it draws them in: a frame whose look
/// matches the last one needs no redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// The level leaving each out jack, from 0 to [`GLOW_STEPS`].
    carried: [u8; SOURCES],
    echo_removed: u8,
    noise_removed: u8,
    reduction: u8,
    limiting: bool,
}

impl Look {
    fn of(r: &DspReading) -> Self {
        let steps = |t: f32, n: f32| (t.clamp(0., 1.) * n).floor() as u8;
        let removed = |from: f32, to: f32| steps((from - to) / REMOVED_FULL_DB, REMOVED_STEPS);
        Look {
            carried: carried(r).map(|db| {
                steps(
                    (db - GLOW_FLOOR_DB) / (GLOW_FULL_DB - GLOW_FLOOR_DB),
                    GLOW_STEPS,
                )
            }),
            echo_removed: removed(r.mic_db, r.after_echo_db),
            noise_removed: removed(r.after_echo_db, r.after_noise_db),
            reduction: steps(r.reduction_db / REDUCTION_FULL_DB, REDUCTION_STEPS),
            limiting: r.limit_db > LIMIT_LIT_DB,
        }
    }

    /// How loud the voice leaving source `s` is, from 0 to 1.
    fn level(&self, s: usize) -> f32 {
        f32::from(self.carried[s]) / GLOW_STEPS
    }

    fn ripples(&self) -> Ripples {
        Ripples {
            level: self.level(0),
            removed: f32::from(self.echo_removed) / REMOVED_STEPS,
        }
    }

    fn grain(&self) -> Grain {
        Grain {
            level: self.level(2),
            removed: f32::from(self.noise_removed) / REMOVED_STEPS,
            seed: u32::from_le_bytes([self.carried[1], self.carried[2], self.noise_removed, 0x5a]),
        }
    }

    fn reduction(&self) -> f32 {
        f32::from(self.reduction) / REDUCTION_STEPS
    }

    /// The steps as the automation model shows them.
    pub fn steps(&self) -> [u8; 8] {
        let [a, b, c, d] = self.carried;
        [
            a,
            b,
            c,
            d,
            self.echo_removed,
            self.noise_removed,
            self.reduction,
            u8::from(self.limiting),
        ]
    }
}

/// What the rack shows: the Settings' patch, and the levels while the voice goes through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RackModel {
    pub direct: bool,
    pub echo: bool,
    pub noise: NoiseSuppression,
    pub compressor: Compressor,
    look: Option<Look>,
}

/// Where one source's cable goes: the module whose in jack takes it (in between two while it
/// swings), and how far it is plugged in (0 while its module is out of the path).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cable {
    to: f32,
    plugged: f32,
}

impl RackModel {
    pub fn new(settings: &Settings, levels: Option<DspReading>) -> Self {
        Self {
            direct: settings.studio_mode,
            echo: settings.echo_cancellation,
            noise: settings.noise_suppression,
            compressor: settings.compressor,
            look: Self::drawn(levels),
        }
    }

    pub fn live(&self) -> bool {
        self.look.is_some()
    }

    fn on(&self, m: Module) -> bool {
        match m {
            Module::Echo => self.echo,
            Module::Noise => self.noise.on,
            Module::Comp => self.compressor.on,
            Module::Mic | Module::Out => true,
        }
    }

    fn in_path(&self, m: Module) -> bool {
        matches!(m, Module::Mic | Module::Out) || (!self.direct && self.on(m))
    }

    /// The look on a module, while the voice goes through it.
    fn look_at(&self, m: Module) -> Option<Look> {
        self.look.filter(|_| self.in_path(m))
    }

    /// Each source's cable runs to the next module that is on. Direct takes MIC's to OUT and
    /// unplugs the rest where they are.
    fn patch(&self) -> [Cable; SOURCES] {
        std::array::from_fn(|s| {
            let next_on = (s + 1..SOURCES)
                .find(|&m| self.on(MODULES[m]))
                .unwrap_or(SOURCES);
            let to = if s == 0 && self.direct {
                SOURCES
            } else {
                next_on
            };
            Cable {
                to: to as f32,
                plugged: if self.in_path(MODULES[s]) { 1. } else { 0. },
            }
        })
    }

    pub fn drawn(levels: Option<DspReading>) -> Option<Look> {
        levels.as_ref().map(Look::of)
    }
}

/// The microphone's level as GAIN shows it, in whole percent as Windows does.
pub fn gain_percent(level: f32) -> u8 {
    (level.clamp(0., 1.) * f32::from(KNOB_MAX)).round() as u8
}

fn ease_out_quint(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(5)
}

/// Where the modules and their jacks are, in design pixels from the rack's top left.
struct Geometry {
    layout: RackLayout,
    width: f32,
}

#[derive(Clone, Copy, Default)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// A jack: where it is, and which way a cable leaves it.
#[derive(Clone, Copy)]
struct Jack {
    at: (f32, f32),
    dir: (f32, f32),
}

const UP: (f32, f32) = (0., -1.);
const DOWN: (f32, f32) = (0., 1.);
const LEFT: (f32, f32) = (-1., 0.);

impl Geometry {
    fn rects(&self) -> [Rect; 5] {
        match self.layout {
            RackLayout::Wide => {
                let mut rects = [Rect::default(); 5];
                let total: f32 = TOP_SHARES.iter().sum();
                let room = self.width - GAP * (TOP_SHARES.len() - 1) as f32;
                let mut x = 0.;
                for (i, share) in TOP_SHARES.iter().enumerate() {
                    let w = room * share / total;
                    rects[i + 1] = Rect {
                        x,
                        y: 0.,
                        w,
                        h: TOP_H,
                    };
                    x += w + GAP;
                }
                let w = (self.width - GAP) / 2.;
                let y = TOP_H + CHANNEL_H;
                rects[0] = Rect {
                    x: 0.,
                    y,
                    w,
                    h: IO_H,
                };
                rects[4] = Rect {
                    x: w + GAP,
                    y,
                    w,
                    h: IO_H,
                };
                rects
            }
            RackLayout::Stacked => {
                let mut y = 0.;
                STACK_H.map(|h| {
                    let rect = Rect {
                        x: GUTTER,
                        y,
                        w: self.width - GUTTER,
                        h,
                    };
                    y += h + GAP;
                    rect
                })
            }
        }
    }

    fn in_jack(&self, m: Module, r: Rect) -> Jack {
        let (left, right, top, bottom) = (
            r.x + JACK_INSET,
            r.x + r.w - JACK_INSET,
            r.y + JACK_INSET,
            r.y + r.h - JACK_INSET,
        );
        match (self.layout, m) {
            (RackLayout::Wide, Module::Out) => Jack {
                at: (right, top),
                dir: UP,
            },
            (RackLayout::Wide, _) => Jack {
                at: (left, bottom),
                dir: DOWN,
            },
            (RackLayout::Stacked, _) => Jack {
                at: (left, top),
                dir: LEFT,
            },
        }
    }

    fn out_jack(&self, m: Module, r: Rect) -> Jack {
        let (left, right, top, bottom) = (
            r.x + JACK_INSET,
            r.x + r.w - JACK_INSET,
            r.y + JACK_INSET,
            r.y + r.h - JACK_INSET,
        );
        match (self.layout, m) {
            (RackLayout::Wide, Module::Mic) => Jack {
                at: (left, top),
                dir: UP,
            },
            (RackLayout::Wide, _) => Jack {
                at: (right, bottom),
                dir: DOWN,
            },
            (RackLayout::Stacked, _) => Jack {
                at: (left, bottom),
                dir: LEFT,
            },
        }
    }

    /// The in jack at a module index, or between two while a plug travels.
    fn in_jack_at(&self, rects: &[Rect; 5], to: f32) -> Jack {
        let to = to.clamp(1., SOURCES as f32);
        let (low, high) = (to.floor() as usize, to.ceil() as usize);
        let (a, b) = (
            self.in_jack(MODULES[low], rects[low]),
            self.in_jack(MODULES[high], rects[high]),
        );
        let t = to - low as f32;
        let lerp = |p: (f32, f32), q: (f32, f32)| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
        Jack {
            at: lerp(a.at, b.at),
            dir: lerp(a.dir, b.dir),
        }
    }

    /// How far a cable over `span` hangs (wide) or loops out (stacked).
    fn slack(&self, span: f32) -> f32 {
        match self.layout {
            RackLayout::Wide => (SAG_BASE + span * SAG_PER_SPAN).min(SAG_MAX),
            RackLayout::Stacked => (SAG_BASE + span * BULGE_PER_SPAN).min(BULGE_MAX),
        }
    }

    /// The cable's two control points. Between two jacks that face one way it hangs by its slack;
    /// from one row to the other it runs across the channel, bowing out from the middle.
    fn controls(&self, from: Jack, to: Jack, slack: f32) -> [(f32, f32); 2] {
        let facing = from.dir.0 * to.dir.0 + from.dir.1 * to.dir.1;
        let same = ((facing + 1.) / 2.).clamp(0., 1.);
        // A cubic's middle sits three quarters of the way to its control points.
        let hang = slack / 0.75;
        let cross = (to.at.1 - from.at.1).abs() / 2.;
        let k = cross + (hang - cross) * same;
        let side = if from.at.0 < self.width / 2. { -1. } else { 1. };
        let bow = BOW * side * (1. - same);
        let pull = |j: Jack| (j.at.0 + j.dir.0 * k + bow, j.at.1 + j.dir.1 * k);
        [pull(from), pull(to)]
    }
}

/// The rack's whole height: the cables hang within it.
fn height(layout: RackLayout) -> f32 {
    match layout {
        RackLayout::Wide => TOP_H + CHANNEL_H + IO_H,
        RackLayout::Stacked => STACK_H.iter().sum::<f32>() + GAP * (STACK_H.len() - 1) as f32,
    }
}

fn span(a: (f32, f32), b: (f32, f32)) -> f32 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// A cable as drawn this frame: where its plug is, how plugged, and where its slack is. The slack
/// follows the plug a little late and past it, so the cable swings as it settles.
#[derive(Debug, Clone, Copy)]
struct Drawn {
    to: f32,
    plugged: f32,
    slack_to: f32,
}

#[derive(IntoElement)]
pub struct PatchRack {
    model: RackModel,
    layout: RackLayout,
    on_change: Option<Callback<RackChange>>,
    swing: Option<(RackModel, f32)>,
    input: Option<AnyElement>,
    output: Option<AnyElement>,
    meter: Meter,
    gain: MicGain,
    on_gain: Option<Callback<f32>>,
    monitor: bool,
    on_monitor: Option<Callback<bool>>,
}

impl PatchRack {
    pub fn new(model: RackModel, layout: RackLayout) -> Self {
        Self {
            model,
            layout,
            on_change: None,
            swing: None,
            input: None,
            output: None,
            meter: Meter::Level(lobby_client::MicReading::SILENT),
            gain: MicGain::Reading,
            on_gain: None,
            monitor: false,
            on_monitor: None,
        }
    }

    pub fn on_change(
        mut self,
        on_change: impl Fn(RackChange, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(on_change));
        self
    }

    /// The microphone's picker on MIC and the speakers' on OUT.
    pub fn devices(mut self, input: impl IntoElement, output: impl IntoElement) -> Self {
        self.input = Some(input.into_any_element());
        self.output = Some(output.into_any_element());
        self
    }

    /// What MIC's meter shows: the microphone's level, or why it cannot be heard.
    pub fn meter(mut self, meter: Meter) -> Self {
        self.meter = meter;
        self
    }

    /// MIC's GAIN knob: the microphone's Windows input level, from 0 to 1.
    pub fn gain(
        mut self,
        gain: MicGain,
        on_set: impl Fn(f32, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.gain = gain;
        self.on_gain = Some(Rc::new(on_set));
        self
    }

    /// OUT's MONITOR switch: whether the person hears their own voice, in a call or out of one.
    pub fn monitor(
        mut self,
        on: bool,
        on_set: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.monitor = on;
        self.on_monitor = Some(Rc::new(on_set));
        self
    }

    /// The cables partway through a change from `from`, as the gallery shows a swing.
    pub fn swing(mut self, from: RackModel, t: f32) -> Self {
        self.swing = Some((from, t));
        self
    }

    fn change(&self) -> impl Fn(RackChange, &mut Window, &mut App) + 'static {
        let on_change = self.on_change.clone();
        move |change, window, cx| {
            if let Some(on_change) = &on_change {
                on_change(change, window, cx);
            }
        }
    }

    /// The cables this frame: sprung toward the patch, or pinned partway for the gallery.
    fn cables(&self, window: &mut Window, cx: &mut App) -> [Drawn; SOURCES] {
        let target = self.model.patch();
        if let Some((from, t)) = self.swing {
            let start = from.patch();
            let t = ease_out_quint(t);
            return std::array::from_fn(|s| {
                let lerp = |a: f32, b: f32| a + (b - a) * t;
                let to = lerp(start[s].to, target[s].to);
                Drawn {
                    to,
                    plugged: lerp(start[s].plugged, target[s].plugged),
                    slack_to: to,
                }
            });
        }
        std::array::from_fn(|s| {
            let key = |what: &str| format!("rack-cable-{}-{what}", MODULES[s].slug());
            Drawn {
                to: spring(key("to"), target[s].to, SWING, window, cx),
                plugged: spring(key("plugged"), target[s].plugged, UNPLUG, window, cx),
                slack_to: spring(key("slack"), target[s].to, SLACK, window, cx),
            }
        })
    }
}

impl RenderOnce for PatchRack {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let inks = Inks::of(cx.theme());
        let model = self.model;
        let layout = self.layout;
        let scale = window_scale(window);
        let input = self.input.take();
        let output = self.output.take();
        let mic = self.mic(input, inks, scale, window, cx);
        let echo = self.processor(Module::Echo, self.echo_face(inks, scale), inks);
        let noise_face = self.noise_face(inks, scale, window, cx);
        let noise = self.processor(Module::Noise, noise_face, inks);
        let comp_face = self.comp_face(inks, scale, window, cx);
        let comp = self.processor(Module::Comp, comp_face, inks);
        let out = self.out(output, inks, scale, cx);
        let cables = self.cables(window, cx);
        let wiring = canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let width = f32::from(bounds.size.width) / scale;
                let geometry = Geometry { layout, width };
                paint_wiring(&geometry, &model, &cables, inks, bounds, scale, window);
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let body = match layout {
            RackLayout::Wide => {
                let row = |h: f32| div().w_full().h(u(h)).flex().gap(u(GAP));
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(u(CHANNEL_H))
                    .child(row(TOP_H).child(echo).child(noise).child(comp))
                    .child(row(IO_H).child(mic).child(out))
            }
            RackLayout::Stacked => div()
                .w_full()
                .pl(u(GUTTER))
                .flex()
                .flex_col()
                .gap(u(GAP))
                .children([mic, echo, noise, comp, out]),
        };
        div()
            .id("patch-rack")
            .relative()
            .w_full()
            .h(u(height(layout)))
            .child(body)
            .child(wiring)
            .test_support()
    }
}

/// A module's panel, sized for its place in the layout.
fn panel(m: Module, layout: RackLayout, share: f32, inks: Inks) -> Stateful<Div> {
    let i = MODULES.iter().position(|&n| n == m).unwrap_or(0);
    let mut panel = div()
        .id(SharedString::from(format!("rack-module-{}", m.slug())))
        .relative()
        .min_w_0()
        .rounded(u(PANEL_RADIUS))
        .bg(inks.panel)
        .border_1()
        .border_color(inks.edge)
        .p(u(PANEL_PAD));
    match layout {
        RackLayout::Wide => {
            // Shares of the row's width from no basis, as `Geometry::rects` divides it.
            panel.style().flex_grow = Some(share);
            panel.style().flex_shrink = Some(0.);
            panel.flex_basis(u(0.)).h_full()
        }
        RackLayout::Stacked => panel.w_full().h(u(STACK_H[i])).pl(u(2. * JACK_INSET)),
    }
}

fn name(m: Module, inks: Inks) -> Div {
    div()
        .flex_shrink_0()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(inks.text)
        .child(m.name())
}

fn caption(text: &'static str, inks: Inks) -> Div {
    div()
        .flex_shrink_0()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(inks.caption)
        .child(text)
}

impl PatchRack {
    fn dim(&self, m: Module) -> impl Fn(Div) -> Div + use<> {
        let set_aside = !self.model.in_path(m);
        move |d: Div| d.when(set_aside, |d| d.opacity(SET_ASIDE))
    }

    fn power(&self, m: Module, inks: Inks) -> Option<Switch> {
        let model = self.model;
        m.power().map(|(label, detail)| {
            let change = self.change();
            Switch::new(SharedString::from(format!("rack-{}", m.slug())))
                .small()
                .checked(model.on(m))
                .color(inks.caption)
                .accessibility_label(label)
                .tooltip(detail)
                .disabled(model.direct)
                .on_click(move |on: &bool, window, cx| {
                    let on = *on;
                    change(
                        match m {
                            Module::Echo => RackChange::Echo(on),
                            Module::Noise => RackChange::Noise(on),
                            _ => RackChange::Compressor(on),
                        },
                        window,
                        cx,
                    )
                })
        })
    }

    /// ECHO, NOISE or COMP: the name and the switch over the face. Wide, the jacks take the foot.
    fn processor(&self, m: Module, face: AnyElement, inks: Inks) -> AnyElement {
        let dim = self.dim(m);
        let share = match m {
            Module::Echo => TOP_SHARES[0],
            Module::Noise => TOP_SHARES[1],
            _ => TOP_SHARES[2],
        };
        let header = div()
            .h(u(HEADER_H))
            .flex()
            .items_center()
            .justify_between()
            .child(dim(div()).child(name(m, inks)))
            .children(self.power(m, inks));
        panel(m, self.layout, share, inks)
            .flex()
            .flex_col()
            .gap(u(FACE_GAP))
            .child(header)
            .child(dim(div()).flex_1().min_h_0().child(face))
            .when(self.layout == RackLayout::Wide, |d| {
                d.child(div().h(u(JACK_BAND - FACE_GAP)).flex_shrink_0())
            })
            .test_support()
            .into_any_element()
    }

    fn echo_face(&self, inks: Inks, scale: f32) -> AnyElement {
        let now = self.model.look_at(Module::Echo).map(|l| l.ripples());
        div()
            .id("rack-ripples")
            .size_full()
            .child(rack_faces::ripples(now, inks, scale))
            .test_support()
            .into_any_element()
    }

    fn noise_face(&self, inks: Inks, scale: f32, window: &mut Window, cx: &mut App) -> AnyElement {
        let model = self.model;
        let focus = window
            .use_keyed_state("rack-noise-engine-focus", cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let mut choice = Segmented::new("rack-noise-engine")
            .compact()
            .focus(&focus)
            .accessibility_label(copy::NOISE_ENGINE);
        for engine in NoiseEngine::ALL {
            let change = self.change();
            let enabled = !model.direct;
            let chosen = engine == model.noise.engine;
            choice = choice
                .word(engine.label(), chosen, move |window, cx| {
                    if enabled {
                        change(RackChange::Engine(engine), window, cx)
                    }
                })
                .tip(engine.detail());
        }
        let now = model.look_at(Module::Noise).map(|l| l.grain());
        let display = div()
            .id("rack-grain")
            .min_w_0()
            .child(rack_faces::grain(now, inks, scale))
            .test_support();
        match self.layout {
            RackLayout::Wide => div()
                .size_full()
                .flex()
                .flex_col()
                .gap(u(FACE_GAP))
                .child(display.w_full().flex_1().min_h_0())
                .child(choice),
            RackLayout::Stacked => div()
                .size_full()
                .flex()
                .items_center()
                .gap(u(8.))
                .child(display.flex_1().h_full())
                .child(div().flex_shrink_0().child(choice)),
        }
        .into_any_element()
    }

    fn comp_face(&self, inks: Inks, scale: f32, window: &mut Window, cx: &mut App) -> AnyElement {
        let model = self.model;
        let change = self.change();
        let knob = rack_knob::knob(
            Dial {
                id: "rack-amount",
                label: copy::COMPRESSOR_AMOUNT,
                tooltip: None,
                value: model.compressor.amount.get(),
                enabled: !model.direct,
            },
            inks,
            Rc::new(move |to, window, cx| {
                change(RackChange::Amount(CompressorAmount::new(to)), window, cx)
            }),
            window,
            cx,
        );
        let reduction = model.look_at(Module::Comp).map(|l| l.reduction());
        let meter = div()
            .id("rack-reduction")
            .min_w_0()
            .child(rack_faces::needle(reduction, inks, scale))
            .test_support();
        div()
            .size_full()
            .flex()
            .items_center()
            .gap(u(8.))
            .child(meter.flex_1().h_full())
            .child(knob)
            .into_any_element()
    }

    /// GAIN, the microphone's own level: at rest and set aside until Windows says what it is.
    fn gain_knob(&self, inks: Inks, window: &mut Window, cx: &mut App) -> Div {
        let (value, tooltip, enabled) = match &self.gain {
            MicGain::Level(level) => (gain_percent(*level), copy::MIC_GAIN_DETAIL.into(), true),
            MicGain::Reading => (0, copy::MIC_GAIN_DETAIL.into(), false),
            MicGain::Unavailable(why) => (0, why.to_string().into(), false),
        };
        let on_gain = self.on_gain.clone();
        let knob = rack_knob::knob(
            Dial {
                id: "rack-gain",
                label: copy::MIC_GAIN,
                tooltip: Some(tooltip),
                value,
                enabled,
            },
            inks,
            Rc::new(move |to, window, cx| {
                if let Some(on_gain) = &on_gain {
                    on_gain(f32::from(to) / f32::from(KNOB_MAX), window, cx);
                }
            }),
            window,
            cx,
        );
        div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .when(!enabled, |d| d.opacity(SET_ASIDE))
            .child(knob)
            .child(caption(copy::RACK_GAIN, inks))
    }

    /// MIC: the microphone's picker and meter on a grille, GAIN beside them, Direct in the corner.
    fn mic(
        &self,
        input: Option<AnyElement>,
        inks: Inks,
        scale: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let model = self.model;
        let change = self.change();
        let direct = div()
            .flex()
            .items_center()
            .gap(u(6.))
            .child(caption(copy::RACK_DIRECT, inks))
            .child(
                Switch::new("rack-direct")
                    .small()
                    .checked(model.direct)
                    .color(inks.caption)
                    .accessibility_label(copy::DIRECT)
                    .tooltip(copy::DIRECT_DETAIL)
                    .on_click(move |on: &bool, window, cx| {
                        change(RackChange::Direct(*on), window, cx)
                    }),
            );
        let wide = self.layout == RackLayout::Wide;
        // Stacked, the jack's band is the panel's own left padding.
        let left = if wide { PANEL_PAD } else { 2. * JACK_INSET };
        let grille = div()
            .absolute()
            .top(u(PANEL_PAD + HEADER_H + FACE_GAP))
            .left(u(left))
            .right(u(PANEL_PAD))
            .bottom(u(PANEL_PAD))
            .child(rack_faces::grille(inks, scale));
        let header = div()
            .h(u(HEADER_H))
            .flex()
            .items_center()
            .gap(u(6.))
            .when(wide, |d| d.child(div().w(u(JACK_BAND)).flex_shrink_0()))
            .child(name(Module::Mic, inks))
            .child(div().flex_1())
            .child(direct);
        panel(Module::Mic, self.layout, 1., inks)
            .child(grille)
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap(u(FACE_GAP))
                    .child(header)
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .gap(u(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(u(FACE_GAP))
                                    .child(div().w_full().children(input))
                                    .child(MicMeterBar::new(self.meter.clone())),
                            )
                            .child(self.gain_knob(inks, window, cx)),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    /// OUT: the speaker cone, the speakers' picker, MONITOR, and the limiter's light.
    fn out(&self, output: Option<AnyElement>, inks: Inks, scale: f32, cx: &App) -> AnyElement {
        let model = self.model;
        let look = model.look_at(Module::Out);
        let lit = !model.direct && look.is_some_and(|l| l.limiting);
        let limit = div()
            .id("rack-limit")
            .flex()
            .items_center()
            .gap(u(2.))
            .child(caption(copy::RACK_LIMIT, inks))
            .child(rack_faces::led(lit, inks, scale))
            .test_support();
        let cone = |d: f32| {
            div()
                .id("rack-cone")
                .size(u(d))
                .flex_shrink_0()
                .child(rack_faces::cone(look.map(|l| l.level(3)), inks, scale))
                .test_support()
        };
        let on_monitor = self.on_monitor.clone();
        let monitor = div()
            .flex()
            .items_center()
            .gap(u(6.))
            .child(caption(copy::RACK_MONITOR, inks))
            .child(
                Switch::new("rack-monitor")
                    .small()
                    .checked(self.monitor)
                    .color(inks.caption)
                    .accessibility_label(copy::MONITOR)
                    .tooltip(copy::MONITOR_DETAIL)
                    .on_click(move |on: &bool, window, cx| {
                        if let Some(on_monitor) = &on_monitor {
                            on_monitor(*on, window, cx);
                        }
                    }),
            );
        let note = div()
            .id("monitor-headphones")
            .h(u(HEADER_H))
            .text_xs()
            .truncate()
            .text_color(cx.theme().muted_foreground)
            .when(self.monitor, |d| d.child(copy::MONITOR_HEADPHONES))
            .test_support();
        let picker = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(u(4.))
            .child(div().w_full().children(output))
            .child(note);
        let wide = self.layout == RackLayout::Wide;
        let header = div()
            .h(u(HEADER_H))
            .flex()
            .items_center()
            .gap(u(6.))
            .child(name(Module::Out, inks))
            .child(div().flex_1())
            .child(monitor)
            .child(limit)
            .when(wide, |d| d.child(div().w(u(JACK_BAND)).flex_shrink_0()));
        panel(Module::Out, self.layout, 1., inks)
            .flex()
            .flex_col()
            .gap(u(FACE_GAP))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .gap(u(10.))
                    .child(cone(if wide { OUT_CONE } else { STACK_CONE }))
                    .child(picker),
            )
            .test_support()
            .into_any_element()
    }
}

/// The jacks, then the cables over them, then the plugs over the cables' ends.
fn paint_wiring(
    geometry: &Geometry,
    model: &RackModel,
    cables: &[Drawn; SOURCES],
    inks: Inks,
    bounds: Bounds<Pixels>,
    scale: f32,
    window: &mut Window,
) {
    let pen = Pen {
        origin: bounds.origin,
        scale,
    };
    let rects = geometry.rects();
    for (i, &m) in MODULES.iter().enumerate() {
        let alpha = if model.in_path(m) { 1. } else { SET_ASIDE };
        let jacks = [
            m.has_in().then(|| geometry.in_jack(m, rects[i])),
            m.has_out().then(|| geometry.out_jack(m, rects[i])),
        ];
        for jack in jacks.into_iter().flatten() {
            circle(
                window,
                pen.at(jack.at),
                pen.d(JACK_R),
                inks.ring.opacity(alpha),
            );
            circle(
                window,
                pen.at(jack.at),
                pen.d(JACK_HOLE_R),
                inks.hole.opacity(alpha),
            );
        }
    }
    let mut order: Vec<usize> = (0..SOURCES).collect();
    order.sort_by(|a, b| cables[*a].plugged.total_cmp(&cables[*b].plugged));
    for s in order {
        let cable = cables[s];
        if cable.plugged <= 0.01 {
            continue;
        }
        let start = geometry.out_jack(MODULES[s], rects[s]);
        let end = geometry.in_jack_at(&rects, cable.to);
        let slack_end = geometry.in_jack_at(&rects, cable.slack_to);
        let slack = geometry.slack(span(start.at, slack_end.at));
        let [c1, c2] = geometry.controls(start, end, slack);
        let points = [pen.at(start.at), pen.at(c1), pen.at(c2), pen.at(end.at)];
        let level = model.look.map(|l| l.level(s));
        let live = match level {
            Some(level) => inks.voice.mix_oklab(inks.panel, 0.5 + 0.5 * level),
            None => inks.rest,
        };
        let color = live
            .mix_oklab(inks.rest, cable.plugged)
            .opacity(cable.plugged);
        if let Some(level) = level {
            let glow = level * cable.plugged;
            if glow > 0. {
                stroke(
                    window,
                    &points,
                    pen.d(GLOW_W),
                    inks.voice.opacity(GLOW_ALPHA * glow),
                );
            }
        }
        let rim = inks.rim.opacity(cable.plugged);
        stroke(window, &points, pen.d(CABLE_W + 2. * CABLE_RIM), rim);
        stroke(window, &points, pen.d(CABLE_W), color);
        for end in [points[0], points[3]] {
            circle(window, end, pen.d(PLUG_R), rim);
            circle(window, end, pen.d(PLUG_CORE_R), color);
        }
    }
}
