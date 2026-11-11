//! Settings' Voice section, in three groups under small headings. Microphone: its select, the
//! live level meter beside the Hear yourself button, a line kept for what is wrong, and the
//! microphone's Windows volume. Speakers: its select. Sound: the three clean-up steps (echo
//! cancellation, noise suppression, the compressor), then the UI sounds. Noise suppression and
//! the compressor are each one Off, Gentle or Strong select, mapped onto the settings they store
//! ([`Strength`]). A device choice is written through `Lobbies` at once, and the call that is on
//! moves to it. A switch, select or slider is written through `Lobbies` too, and the Shell hands
//! the call that is on what was saved.
//!
//! The lists come from a [`DeviceWatch`] that lives as long as the page is open, and so do the
//! microphone test and its volume: the meter opens the chosen microphone on a stream of its own,
//! except in a call, where it reads the level the call is already measuring. Mic volume reads and
//! sets the chosen microphone's Windows level, found in the watch's lists. Hear yourself (Monitor)
//! is heard in the call that is on; with none on, the page's test plays the microphone through a
//! call's chain to the speakers instead. It goes off when the page closes.

use std::{rc::Rc, time::Duration};

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, RenderOnce, SharedString, Styled as _, Subscription, Task,
    TestSupportExt as _, Window,
    base::slider::{SliderEvent, SliderState},
    div,
    prelude::FluentBuilder as _,
};
use lobby_client::{
    Compressor, CompressorAmount, DEVICE_POLL, DeviceChoice, DeviceKind, DeviceList, DeviceSetup,
    DeviceWatch, Endpoints, Intent, MediaError, MicGain, MicLevel, MicMeter, MicReading, MicTest,
    MicVolume, MonitorControl, NoiseEngine, NoiseSuppression, Settings, VoicePhase, copy,
};

use crate::{
    components::{
        Meter, MicMeterBar, PERCENT_W, READOUT_GAP, lit_segments, mic_hint, page_select,
        truncated_text,
    },
    kit::{
        ActiveTheme as _,
        searchable_list::{SearchableListDelegate, SearchableListItem, SearchableVec},
        select::{SelectEvent, SelectState},
    },
    lobbies::Lobbies,
    lobby_view::VoiceDevices,
    scale::u,
    session_entity::SessionEntity,
    settings_view::{GROUP_GAP, group, row, section},
    ui,
    voice_shared::VoiceShared,
};

pub type Items = SearchableVec<DeviceOption>;

/// How often the meter redraws, about 30 times a second, and the section takes a new device list
/// from its watch.
const METER_FRAME: Duration = Duration::from_millis(33);
/// Under `LOBBY_FAKE_MEDIA=1` there is no microphone: the meter measures this steady sample
/// (-20 dBFS, a good level) at the rate below.
const FAKE_SAMPLE: f32 = 0.1;
const FAKE_RATE: u32 = 48_000;
/// Fake media's microphone has a volume of its own, so Input level never sets a real microphone's.
const FAKE_GAIN: f32 = 0.8;
/// The slider runs in whole percent, as Windows shows the input level.
const PERCENT_MAX: f32 = 100.;

const SLIDER_W: f32 = 140.;
/// The Off, Gentle or Strong selects beside their rows' words.
const STRENGTH_W: f32 = 136.;
/// Where a stored compressor amount stops reading as Gentle and starts reading as Strong: the
/// middle of the two choices.
const STRONG_FROM: u8 = (CompressorAmount::DEFAULT.get() + CompressorAmount::STRONG.get()) / 2;
/// What a control not usable yet fades back to.
const SET_ASIDE: f32 = 0.45;
/// The meter and its button on one line, and the hint slot closer under them than rows are.
const LISTEN_GAP: f32 = 12.;
const LISTEN_HINT_GAP: f32 = 4.;

/// What the selects list before the watch's first read.
static NOT_READ: Endpoints = Endpoints {
    devices: Vec::new(),
    default: None,
    communications: None,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceOption {
    pub choice: DeviceChoice,
    pub label: SharedString,
}

impl SearchableListItem for DeviceOption {
    type Value = DeviceChoice;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &DeviceChoice {
        &self.choice
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(truncated_text("device-shown", self.label.clone()).into_any_element())
    }
}

/// The select's lines: the two defaults, then each device by name. A chosen device that is not
/// listed (unplugged) is kept as the last line, so the select still says what is chosen.
pub fn options(endpoints: &Endpoints, chosen: &DeviceChoice) -> Vec<DeviceOption> {
    let option = |choice, label: &str| DeviceOption {
        choice,
        label: label.to_owned().into(),
    };
    let mut lines = vec![option(
        DeviceChoice::SystemDefault,
        copy::DEVICE_SYSTEM_DEFAULT,
    )];
    if endpoints.has_communications() {
        lines.push(option(
            DeviceChoice::Communications,
            copy::DEVICE_COMMUNICATIONS,
        ));
    }
    lines.extend(
        endpoints
            .devices
            .iter()
            .map(|d| option(DeviceChoice::Named(d.clone()), &d.name)),
    );
    if let DeviceChoice::Named(wanted) = chosen
        && endpoints.get(&wanted.id).is_none()
    {
        lines.push(option(
            chosen.clone(),
            &format!("{} ({})", wanted.name, copy::DEVICE_NOT_CONNECTED),
        ));
    }
    lines
}

/// What the page opens the microphone for while no call has it open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listening {
    /// The microphone row's meter alone.
    Meter,
    /// Monitor: through a call's chain to the speakers.
    Monitor,
}

/// How much of a clean-up step to apply. Settings stores each step its own way (a noise engine, a
/// compressor amount); this is the one scale a person picks from, and the only place that maps
/// the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    Off,
    Gentle,
    Strong,
}

impl Strength {
    const ALL: [Strength; 3] = [Strength::Off, Strength::Gentle, Strength::Strong];

    fn label(self) -> &'static str {
        match self {
            Strength::Off => "Off",
            Strength::Gentle => "Gentle",
            Strength::Strong => "Strong",
        }
    }

    fn of_noise(noise: NoiseSuppression) -> Self {
        match (noise.on, noise.engine) {
            (false, _) => Strength::Off,
            (true, NoiseEngine::Webrtc) => Strength::Gentle,
            (true, NoiseEngine::Rnnoise) => Strength::Strong,
        }
    }

    /// A stored amount reads as the nearer of Gentle and Strong.
    fn of_compressor(compressor: Compressor) -> Self {
        match (compressor.on, compressor.amount.get() < STRONG_FROM) {
            (false, _) => Strength::Off,
            (true, true) => Strength::Gentle,
            (true, false) => Strength::Strong,
        }
    }

    /// Off keeps the engine, so turning it back on finds the last choice.
    fn set_noise(self, noise: &mut NoiseSuppression) {
        noise.on = self != Strength::Off;
        match self {
            Strength::Off => {}
            Strength::Gentle => noise.engine = NoiseEngine::Webrtc,
            Strength::Strong => noise.engine = NoiseEngine::Rnnoise,
        }
    }

    fn set_compressor(self, compressor: &mut Compressor) {
        compressor.on = self != Strength::Off;
        match self {
            Strength::Off => {}
            Strength::Gentle => compressor.amount = CompressorAmount::DEFAULT,
            Strength::Strong => compressor.amount = CompressorAmount::STRONG,
        }
    }
}

impl SearchableListItem for Strength {
    type Value = Strength;

    fn title(&self) -> SharedString {
        self.label().into()
    }

    fn value(&self) -> &Strength {
        self
    }
}

pub type Strengths = SearchableVec<Strength>;

/// The section's controls that keep a state of their own: the microphone's input level slider in
/// whole percent, and the selects for noise suppression and the compressor.
#[derive(Clone)]
pub struct VoiceControls {
    gain: Entity<SliderState>,
    noise: Entity<SelectState<Strengths>>,
    compressor: Entity<SelectState<Strengths>>,
}

impl VoiceControls {
    pub fn new(gain_percent: f32, window: &mut Window, cx: &mut App) -> Self {
        let gain = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(PERCENT_MAX)
                .step(1.)
                .default_value(gain_percent)
        });
        let strength = |window: &mut Window, cx: &mut App| {
            cx.new(|cx| SelectState::new(Strengths::new(Strength::ALL.to_vec()), None, window, cx))
        };
        Self {
            gain,
            noise: strength(window, cx),
            compressor: strength(window, cx),
        }
    }
}

/// Puts the select on `shown`, unless it already stands there. The section does it as it draws,
/// so the settings it is given decide what the selects say.
fn show_strength(
    select: &Entity<SelectState<Strengths>>,
    shown: Strength,
    window: &mut Window,
    cx: &mut App,
) {
    if select.read(cx).selected_value() != Some(&shown) {
        select.update(cx, |select, cx| {
            select.set_selected_value(&shown, window, cx)
        });
    }
}

pub struct VoiceSettingsView {
    lobbies: Entity<Lobbies>,
    devices: VoiceDevices,
    session: Option<Entity<SessionEntity>>,
    level: MicLevel,
    watch: DeviceWatch,
    /// None until the watch's first read.
    list: Option<DeviceList>,
    input: Entity<SelectState<Items>>,
    output: Entity<SelectState<Items>>,
    controls: VoiceControls,
    test: Option<(Listening, MicTest)>,
    fake: Option<MicMeter>,
    /// The chosen microphone's Windows volume.
    volume: MicVolume,
    monitor: MonitorControl,
    shown: MicReading,
    gain_shown: MicGain,
    failure: Option<MediaError>,
    in_call: bool,
    _metering: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl VoiceSettingsView {
    pub fn new(
        lobbies: Entity<Lobbies>,
        devices: VoiceDevices,
        session: Option<Entity<SessionEntity>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let VoiceShared { level, monitor, .. } = VoiceShared::of(cx);
        let watch = DeviceWatch::start(devices.source(), DEVICE_POLL);
        let list = watch.take();
        let input = cx.new(|cx| SelectState::new(Items::new(Vec::new()), None, window, cx));
        let output = cx.new(|cx| SelectState::new(Items::new(Vec::new()), None, window, cx));
        let controls = VoiceControls::new(0., window, cx);
        let mut subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, ev: &SelectEvent<Items>, w, cx| {
                this.picked(DeviceKind::Input, ev, w, cx)
            }),
            cx.subscribe_in(
                &output,
                window,
                |this, _, ev: &SelectEvent<Items>, w, cx| {
                    this.picked(DeviceKind::Output, ev, w, cx)
                },
            ),
            cx.subscribe(&controls.gain, |this, _, ev: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = ev {
                    this.change(VoiceChange::Gain(value.start() / PERCENT_MAX), cx);
                }
            }),
            cx.subscribe(
                &controls.noise,
                |this, _, ev: &SelectEvent<Strengths>, cx| {
                    if let SelectEvent::Confirm(Some(strength)) = ev {
                        this.change(VoiceChange::Noise(*strength), cx);
                    }
                },
            ),
            cx.subscribe(
                &controls.compressor,
                |this, _, ev: &SelectEvent<Strengths>, cx| {
                    if let SelectEvent::Confirm(Some(strength)) = ev {
                        this.change(VoiceChange::Compressor(*strength), cx);
                    }
                },
            ),
            cx.observe_in(&lobbies, window, |this, _, window, cx| {
                this.refresh(window, cx)
            }),
        ];
        if let Some(session) = &session {
            subscriptions.push(cx.observe(session, |this, _, cx| this.sync_test(cx)));
        }
        let metering = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(METER_FRAME).await;
                if this
                    .update_in(cx, |v, window, cx| v.meter_frame(window, cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        let fake = (devices == VoiceDevices::Fake).then(|| MicMeter::new(FAKE_RATE, level.clone()));
        let volume = match devices {
            VoiceDevices::System => MicVolume::start(),
            // Fake media's microphone is no device, so it gets a volume that touches none.
            VoiceDevices::Fake => MicVolume::fake(FAKE_GAIN),
        };
        let mut view = Self {
            lobbies,
            devices,
            session,
            level,
            watch,
            list,
            input,
            output,
            controls,
            test: None,
            fake,
            volume,
            monitor,
            shown: MicReading::SILENT,
            gain_shown: MicGain::Reading,
            failure: None,
            in_call: false,
            _metering: metering,
            _subscriptions: subscriptions,
        };
        view.refresh(window, cx);
        view.sync_test(cx);
        view
    }

    fn select(&self, kind: DeviceKind) -> &Entity<SelectState<Items>> {
        match kind {
            DeviceKind::Input => &self.input,
            DeviceKind::Output => &self.output,
        }
    }

    /// The lines of a select and the one shown, for the automation model.
    #[cfg(feature = "automation")]
    pub fn dump(&self, cx: &App) -> serde_json::Value {
        let settings = self.lobbies.read(cx).settings();
        let side = |kind: DeviceKind| {
            let chosen = self.chosen(kind, cx);
            serde_json::json!({
                "lines": options(self.endpoints(kind), &chosen)
                    .iter()
                    .map(|o| o.label.to_string())
                    .collect::<Vec<_>>(),
                "shown": self.select(kind).read(cx).selected_value().cloned().map(|c| format!("{c:?}")),
            })
        };
        serde_json::json!({
            "input": side(DeviceKind::Input),
            "output": side(DeviceKind::Output),
            "noise_suppression": {
                "on": settings.noise_suppression.on,
                "engine": settings.noise_suppression.engine.label(),
            },
            "echo_cancellation": settings.echo_cancellation,
            "compressor": {
                "on": settings.compressor.on,
                "amount": settings.compressor.amount.get(),
            },
            "gain": match &self.gain_shown {
                MicGain::Level(level) => serde_json::json!((level.clamp(0., 1.) * PERCENT_MAX).round() as u8),
                other => serde_json::json!(format!("{other:?}")),
            },
            "monitor": self.monitor.is_on(),
            "sounds": settings.sounds,
            "meter": lit_segments(self.shown.level_db),
            "mic_quality": format!("{:?}", self.shown.quality),
            "failure": self.failure.as_ref().map(ToString::to_string),
            "in_call": self.in_call,
            "listening": self.test.as_ref().map(|(listening, _)| format!("{listening:?}")),
        })
    }

    fn chosen(&self, kind: DeviceKind, cx: &App) -> DeviceChoice {
        let settings = self.lobbies.read(cx).settings();
        match kind {
            DeviceKind::Input => settings.input_device.clone(),
            DeviceKind::Output => settings.output_device.clone(),
        }
    }

    fn endpoints(&self, kind: DeviceKind) -> &Endpoints {
        self.list.as_ref().map_or(&NOT_READ, |list| list.of(kind))
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for kind in [DeviceKind::Input, DeviceKind::Output] {
            let chosen = self.chosen(kind, cx);
            show_devices(self.select(kind), self.endpoints(kind), &chosen, window, cx);
        }
        if let Some(list) = &self.list {
            let mic = self.chosen(DeviceKind::Input, cx);
            self.volume.follow(&mic, &list.inputs);
        }
        cx.notify();
    }

    fn picked(
        &mut self,
        kind: DeviceKind,
        ev: &SelectEvent<Items>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(choice)) = ev else {
            return;
        };
        self.choose(kind, choice.clone(), cx);
    }

    /// Writes the choice; the call that is on moves to it, and so does the page's test.
    pub fn choose(&mut self, kind: DeviceKind, choice: DeviceChoice, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            let choose = Intent::ChooseDevice {
                kind,
                choice: choice.clone(),
            };
            session.update(cx, |s, cx| s.dispatch(choose, cx));
        }
        self.lobbies.update(cx, |l, cx| {
            l.update_settings(
                |s| match kind {
                    DeviceKind::Input => s.input_device = choice,
                    DeviceKind::Output => s.output_device = choice,
                },
                cx,
            )
        });
        if kind == DeviceKind::Input || self.monitoring_alone() {
            self.test = None;
            self.sync_test(cx);
        }
    }

    fn monitoring_alone(&self) -> bool {
        matches!(self.test, Some((Listening::Monitor, _)))
    }

    fn call_is_on(&self, cx: &App) -> bool {
        self.session
            .as_ref()
            .and_then(|s| s.read(cx).state())
            .is_some_and(|s| s.voice.phase != VoicePhase::Idle)
    }

    /// While the page is open and no call has the microphone open, the page opens it: for Monitor
    /// while it is on, else for the meter. A call that starts takes it over, and hands it back
    /// when it ends.
    fn sync_test(&mut self, cx: &mut Context<Self>) {
        self.in_call = self.call_is_on(cx);
        let wanted = if self.in_call {
            None
        } else if self.monitor.is_on() {
            Some(Listening::Monitor)
        } else {
            (self.devices != VoiceDevices::Fake).then_some(Listening::Meter)
        };
        if self.test.as_ref().map(|(listening, _)| *listening) != wanted {
            // The last one is told to stop before the next one starts.
            self.test = None;
            self.test = wanted.map(|listening| (listening, self.listen(listening, cx)));
        }
        cx.notify();
    }

    fn listen(&self, listening: Listening, cx: &mut App) -> MicTest {
        match listening {
            Listening::Meter => {
                let mic = self.chosen(DeviceKind::Input, cx);
                MicTest::start(
                    DeviceSetup::new(mic, self.devices.source()),
                    self.level.clone(),
                )
            }
            Listening::Monitor => {
                let dsp = VoiceShared::of(cx).dsp;
                let media = self.devices.config(
                    self.lobbies.read(cx).settings(),
                    self.level.clone(),
                    dsp,
                    self.monitor.clone(),
                );
                MicTest::monitored(media)
            }
        }
    }

    fn meter_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(list) = self.watch.take() {
            self.list = Some(list);
            self.refresh(window, cx);
        }
        if let Some(fake) = &mut self.fake {
            for _ in 0..FAKE_RATE / 1000 * METER_FRAME.as_millis() as u32 {
                fake.push(FAKE_SAMPLE);
            }
        }
        let shown = self.level.read();
        let failure = self.test.as_ref().and_then(|(_, test)| test.failure());
        let gain = self.volume.read();
        if drawn_differently(&shown, &self.shown)
            || failure != self.failure
            || gain != self.gain_shown
        {
            self.shown = shown;
            self.failure = failure;
            if let MicGain::Level(level) = &gain {
                // The slider stands where Windows says; it sends no change for this.
                let percent = level * PERCENT_MAX;
                self.controls
                    .gain
                    .update(cx, |slider, cx| slider.set_value(percent, window, cx));
            }
            self.gain_shown = gain;
            cx.notify();
        }
    }

    /// Saved choices are written and heard from the call's next frame; Input level goes to
    /// Windows and Monitor to the call or the page's test, neither of them saved.
    fn change(&mut self, change: VoiceChange, cx: &mut Context<Self>) {
        match change {
            VoiceChange::Gain(level) => {
                self.volume.set(level);
                // Shown at once, so a held key steps on from the level asked, not the last read.
                self.gain_shown = MicGain::Level(level);
            }
            VoiceChange::Monitor(on) => {
                self.monitor.set_on(on);
                self.sync_test(cx);
            }
            VoiceChange::Echo(on) => self.save(|s| s.echo_cancellation = on, cx),
            VoiceChange::Noise(strength) => {
                self.save(|s| strength.set_noise(&mut s.noise_suppression), cx)
            }
            VoiceChange::Compressor(strength) => {
                self.save(|s| strength.set_compressor(&mut s.compressor), cx)
            }
            VoiceChange::Sounds(on) => self.save(|s| s.sounds = on, cx),
        }
        cx.notify();
    }

    fn save(&self, change: impl FnOnce(&mut Settings), cx: &mut App) {
        self.lobbies
            .update(cx, |l, cx| l.update_settings(change, cx));
    }
}

impl Drop for VoiceSettingsView {
    /// Monitor is a tool of the page: no call goes on playing the person to themselves once the
    /// page that turns it off is closed, or follows another Lobby.
    fn drop(&mut self) {
        self.monitor.set_on(false);
    }
}

/// Whether the meter would look any different, so a quiet microphone does not redraw it 20 times
/// a second.
fn drawn_differently(a: &MicReading, b: &MicReading) -> bool {
    a.quality != b.quality
        || lit_segments(a.level_db) != lit_segments(b.level_db)
        || lit_segments(a.peak_db) != lit_segments(b.peak_db)
}

pub fn device_select(
    endpoints: &Endpoints,
    chosen: &DeviceChoice,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SelectState<Items>> {
    let select = cx.new(|cx| SelectState::new(Items::new(Vec::new()), None, window, cx));
    show_devices(&select, endpoints, chosen, window, cx);
    select
}

fn show_devices(
    select: &Entity<SelectState<Items>>,
    endpoints: &Endpoints,
    chosen: &DeviceChoice,
    window: &mut Window,
    cx: &mut App,
) {
    let lines = options(endpoints, chosen);
    select.update(cx, |select, cx| {
        select.set_items(Items::new(lines), window, cx);
        select.set_selected_value(chosen, window, cx);
    });
}

/// What a person changed in the section, past the device selects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VoiceChange {
    Echo(bool),
    Noise(Strength),
    Compressor(Strength),
    /// The microphone's Windows input level, from 0 to 1.
    Gain(f32),
    Monitor(bool),
    Sounds(bool),
}

type OnChange = Rc<dyn Fn(VoiceChange, &mut Window, &mut App)>;

/// The section: the microphone, the speakers, the three clean-up steps, and the UI sounds.
#[derive(IntoElement)]
pub struct VoiceSection {
    input: Entity<SelectState<Items>>,
    output: Entity<SelectState<Items>>,
    controls: VoiceControls,
    meter: Meter,
    echo: bool,
    noise: NoiseSuppression,
    compressor: Compressor,
    sounds: bool,
    gain: MicGain,
    monitor: bool,
    on_change: OnChange,
}

impl VoiceSection {
    pub fn new(
        input: &Entity<SelectState<Items>>,
        output: &Entity<SelectState<Items>>,
        controls: &VoiceControls,
        meter: Meter,
        settings: &Settings,
        on_change: impl Fn(VoiceChange, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            input: input.clone(),
            output: output.clone(),
            controls: controls.clone(),
            meter,
            echo: settings.echo_cancellation,
            noise: settings.noise_suppression,
            compressor: settings.compressor,
            sounds: settings.sounds,
            gain: MicGain::Reading,
            monitor: false,
            on_change: Rc::new(on_change),
        }
    }

    /// What the microphone's volume row shows: its Windows input level.
    pub fn gain(mut self, gain: MicGain) -> Self {
        self.gain = gain;
        self
    }

    pub fn monitor(mut self, on: bool) -> Self {
        self.monitor = on;
        self
    }

    fn switch(
        &self,
        id: &'static str,
        label: &'static str,
        on: bool,
        change: fn(bool) -> VoiceChange,
    ) -> ui::Switch {
        let on_change = self.on_change.clone();
        ui::Switch::new(id)
            .checked(on)
            .accessibility_label(label)
            .on_change(move |on, window, cx| on_change(change(on), window, cx))
    }

    /// The live meter and the toggle that plays it back, with the line that is kept under them
    /// for what is wrong.
    fn listening(&self, cx: &App) -> AnyElement {
        let on = self.monitor;
        let on_change = self.on_change.clone();
        let button = ui::Button::new("monitor")
            .debug_selector(|| "monitor".into())
            .size(ui::Size::Page);
        let button = if on {
            button.variant(ui::Variant::Primary).border_1()
        } else {
            button.variant(ui::Variant::Secondary)
        };
        div()
            .flex()
            .flex_col()
            .gap(u(LISTEN_HINT_GAP))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(u(LISTEN_GAP))
                    .child(MicMeterBar::new(self.meter.clone()))
                    .child(button.label(copy::MONITOR).toggled(on).on_click(
                        move |_, window, cx| on_change(VoiceChange::Monitor(!on), window, cx),
                    )),
            )
            .child(mic_hint(&self.meter, on, cx))
            .into_any_element()
    }

    /// The microphone's volume: a slider with its percent, or why there is none.
    fn input_level(&self, cx: &App) -> AnyElement {
        let usable = match &self.gain {
            MicGain::Unavailable(why) => {
                return div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(why.to_string()))
                    .into_any_element();
            }
            MicGain::Level(_) => true,
            MicGain::Reading => false,
        };
        let percent = self.controls.gain.read(cx).value().start().round() as u16;
        div()
            .flex()
            .items_center()
            .gap(u(READOUT_GAP))
            .when(!usable, |d| d.opacity(SET_ASIDE))
            .child(
                div()
                    .id("input-level")
                    .w(u(SLIDER_W))
                    .child(
                        ui::Slider::new(&self.controls.gain)
                            .disabled(!usable)
                            .accessibility_label(copy::MIC_GAIN),
                    )
                    .test_support(),
            )
            .child(
                div()
                    .w(u(PERCENT_W))
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_sm()
                    .text_right()
                    .child(if usable {
                        format!("{percent}%")
                    } else {
                        String::new()
                    }),
            )
            .into_any_element()
    }

    /// The Off, Gentle or Strong select of a clean-up step, showing `shown`.
    fn strength_select(
        &self,
        id: &'static str,
        label: &'static str,
        state: &Entity<SelectState<Strengths>>,
        shown: Strength,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        show_strength(state, shown, window, cx);
        div()
            .w(u(STRENGTH_W))
            .child(picker(state, id, label))
            .into_any_element()
    }
}

/// A picker, at its row's full width.
pub fn picker<D>(
    state: &Entity<SelectState<D>>,
    id: &'static str,
    label: &'static str,
) -> impl IntoElement + use<D>
where
    D: SearchableListDelegate + 'static,
    <D::Item as SearchableListItem>::Value: PartialEq + Clone,
{
    div()
        .id(id)
        .w_full()
        .child(page_select(state, label))
        .test_support()
}

impl RenderOnce for VoiceSection {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let noise = self.strength_select(
            "noise-suppression",
            copy::NOISE_SUPPRESSION,
            &self.controls.noise,
            Strength::of_noise(self.noise),
            window,
            cx,
        );
        let compressor = self.strength_select(
            "compressor",
            copy::COMPRESSOR,
            &self.controls.compressor,
            Strength::of_compressor(self.compressor),
            window,
            cx,
        );
        let microphone = group(
            copy::DEVICE_INPUT,
            [
                picker(&self.input, "input-device", copy::DEVICE_INPUT).into_any_element(),
                self.listening(cx),
                row(copy::MIC_GAIN, self.input_level(cx), cx).into_any_element(),
            ],
            cx,
        );
        let speakers = group(
            copy::DEVICE_OUTPUT,
            [picker(&self.output, "output-device", copy::DEVICE_OUTPUT).into_any_element()],
            cx,
        );
        let sound = group(
            "Sound",
            [
                row(
                    copy::ECHO_CANCELLATION,
                    self.switch(
                        "echo-cancellation",
                        copy::ECHO_CANCELLATION,
                        self.echo,
                        VoiceChange::Echo,
                    ),
                    cx,
                )
                .into_any_element(),
                row(copy::NOISE_SUPPRESSION, noise, cx).into_any_element(),
                row(copy::COMPRESSOR, compressor, cx).into_any_element(),
            ],
            cx,
        );
        let sounds = row(
            copy::SOUNDS,
            self.switch("sounds", copy::SOUNDS, self.sounds, VoiceChange::Sounds),
            cx,
        );
        section(
            "Voice",
            div()
                .flex()
                .flex_col()
                .gap(u(GROUP_GAP))
                .child(microphone)
                .child(speakers)
                .child(sound)
                .child(sounds),
            cx,
        )
    }
}

impl Render for VoiceSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let meter = match &self.failure {
            Some(failure) => Meter::Failed(SharedString::from(failure.to_string())),
            None => Meter::Level(self.shown),
        };
        let this = cx.entity().downgrade();
        let settings = self.lobbies.read(cx).settings();
        VoiceSection::new(
            &self.input,
            &self.output,
            &self.controls,
            meter,
            settings,
            move |change, _, cx| {
                let _ = this.update(cx, |v, cx| v.change(change, cx));
            },
        )
        .gain(self.gain_shown.clone())
        .monitor(self.monitor.is_on())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_client::{FakeDevices, NamedDevice};

    fn labels(lines: &[DeviceOption]) -> Vec<&str> {
        lines.iter().map(|l| l.label.as_ref()).collect()
    }

    #[test]
    fn the_select_lists_the_defaults_then_the_devices() {
        let list = FakeDevices::sample();
        let lines = options(&list.inputs, &DeviceChoice::SystemDefault);
        assert_eq!(
            labels(&lines),
            [
                "System default",
                "Communications default",
                "Built-in Microphone",
                "Headset Microphone"
            ]
        );
        let none = Endpoints::default();
        assert_eq!(
            labels(&options(&none, &DeviceChoice::SystemDefault)),
            ["System default"],
            "no communications default, no line for it"
        );
    }

    #[test]
    fn an_unplugged_choice_stays_in_the_select_with_its_name() {
        let gone = NamedDevice {
            id: "gone".into(),
            name: "Arctis 7".into(),
        };
        let lines = options(
            &FakeDevices::sample().outputs,
            &DeviceChoice::Named(gone.clone()),
        );
        let last = lines.last().unwrap();
        assert_eq!(last.label, "Arctis 7 (not connected)");
        assert_eq!(last.choice, DeviceChoice::Named(gone));
    }
}
