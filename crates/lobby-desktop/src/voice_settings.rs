//! Settings' Voice section: the rack, from the microphone (its picker and a meter that shows it is
//! heard, on MIC) through what cleans it (echo cancellation, noise suppression, the compressor, or
//! Direct past all of them) to the speakers (their picker, on OUT); and the UI sounds. A device
//! choice is written through `Lobbies` at once, and the call that is on moves to it. The rack
//! applies to the call that is on, as the person patches it, and shows that call's levels.
//!
//! The lists come from a [`DeviceWatch`] that lives as long as the page is open, and so do the
//! microphone test and its volume: the meter opens the chosen microphone on a stream of its own,
//! except in a call, where it reads the level the call is already measuring. GAIN reads and sets
//! the chosen microphone's Windows level. MONITOR is heard in the call that is on; with none on,
//! the page's test plays the microphone through a call's chain to the speakers instead, and the
//! rack shows its levels as a call's.

use std::{rc::Rc, time::Duration};

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Global, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, RenderOnce, SharedString, Styled as _, Subscription,
    Task, TestSupportExt as _, Window,
    component::{
        Sizable as _,
        searchable_list::SearchableListItem,
        select::{Select, SelectEvent, SelectState},
        switch::Switch,
    },
    div,
};
use lobby_client::{
    DEVICE_POLL, DeviceChoice, DeviceKind, DeviceList, DeviceSetup, DeviceWatch, DspControl,
    DspLevels, DspReading, Endpoints, Intent, MediaError, MicGain, MicLevel, MicMeter, MicReading,
    MicTest, MicVolume, MonitorControl, Settings, VoicePhase, copy,
};

use crate::{
    components::{
        Meter, PatchRack, RackChange, RackLayout, RackModel, lit_segments, truncated_text,
    },
    lobbies::Lobbies,
    lobby_view::VoiceDevices,
    scale::u,
    session_entity::SessionEntity,
    settings_view::{row, section},
};

pub type Items = gpui_kit::component::searchable_list::SearchableVec<DeviceOption>;

/// How often the meters redraw, about 30 times a second, and the section takes a new device list
/// from its watch.
const METER_FRAME: Duration = Duration::from_millis(33);
/// Under `LOBBY_FAKE_MEDIA=1` there is no microphone: the meter measures this steady sample
/// (-20 dBFS, a good level) at the rate below.
const FAKE_SAMPLE: f32 = 0.1;
const FAKE_RATE: u32 = 48_000;
/// Fake media's microphone has a volume of its own, so GAIN never sets a real microphone's.
const FAKE_GAIN: f32 = 0.8;
/// A picker's list is wider than the picker on OUT, so a device's whole name shows in it.
const PICKER_MENU_W: f32 = 260.;

/// The one meter of the window, so the call and the page see the same microphone.
#[derive(Default)]
struct SharedLevel(MicLevel);

impl Global for SharedLevel {}

/// What a call measures and the page reads while a call is on.
pub fn mic_level(cx: &mut App) -> MicLevel {
    cx.default_global::<SharedLevel>().0.clone()
}

/// How the microphone is cleaned, shared with the call: a change in Settings reaches the call
/// that is on, and the next Join starts from the saved choice.
#[derive(Default)]
struct SharedDsp(DspControl);

impl Global for SharedDsp {}

pub fn dsp_control(cx: &mut App) -> DspControl {
    cx.default_global::<SharedDsp>().0.clone()
}

fn apply_dsp(lobbies: &Entity<Lobbies>, cx: &mut App) {
    let config = lobbies.read(cx).settings().into();
    dsp_control(cx).set(config);
}

/// What the call measures along the rack, read by the page while a call is on.
#[derive(Default)]
struct SharedDspLevels(DspLevels);

impl Global for SharedDspLevels {}

pub fn dsp_levels(cx: &mut App) -> DspLevels {
    cx.default_global::<SharedDspLevels>().0.clone()
}

/// Whether the person hears themselves, shared with the call like the rack. Never saved: each
/// run of the app starts with it off.
#[derive(Default)]
struct SharedMonitor(MonitorControl);

impl Global for SharedMonitor {}

pub fn monitor_control(cx: &mut App) -> MonitorControl {
    cx.default_global::<SharedMonitor>().0.clone()
}

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
    /// MIC's meter alone.
    Meter,
    /// MONITOR: through a call's chain to the speakers, measured along the rack.
    Monitor,
}

pub struct VoiceSettingsView {
    lobbies: Entity<Lobbies>,
    devices: VoiceDevices,
    session: Option<Entity<SessionEntity>>,
    level: MicLevel,
    dsp_levels: DspLevels,
    watch: DeviceWatch,
    list: DeviceList,
    input: Entity<SelectState<Items>>,
    output: Entity<SelectState<Items>>,
    test: Option<(Listening, MicTest)>,
    fake: Option<MicMeter>,
    /// The chosen microphone's Windows volume, and the choice it follows.
    volume: (DeviceChoice, MicVolume),
    monitor: MonitorControl,
    shown: MicReading,
    /// The call's levels along the rack as last drawn; None outside a call.
    rack_shown: Option<DspReading>,
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
        let level = mic_level(cx);
        let dsp_levels = dsp_levels(cx);
        let watch = DeviceWatch::start(devices.source(), DEVICE_POLL);
        let list = watch.take().unwrap_or_default();
        let input = cx.new(|cx| SelectState::new(Items::new(Vec::new()), None, window, cx));
        let output = cx.new(|cx| SelectState::new(Items::new(Vec::new()), None, window, cx));
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
        let mic = lobbies.read(cx).settings().input_device.clone();
        let volume = (mic.clone(), open_volume(devices, mic));
        let mut view = Self {
            lobbies,
            devices,
            session,
            level,
            dsp_levels,
            watch,
            list,
            input,
            output,
            test: None,
            fake,
            volume,
            monitor: monitor_control(cx),
            shown: MicReading::SILENT,
            rack_shown: None,
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
                "lines": options(self.list.of(kind), &chosen)
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
            "studio_mode": settings.studio_mode,
            "rack": RackModel::drawn(self.rack_shown).map(|look| look.steps()),
            "gain": match &self.gain_shown {
                MicGain::Level(level) => serde_json::json!(crate::components::gain_percent(*level)),
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

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for kind in [DeviceKind::Input, DeviceKind::Output] {
            let chosen = self.chosen(kind, cx);
            show_devices(self.select(kind), self.list.of(kind), &chosen, window, cx);
        }
        let mic = self.chosen(DeviceKind::Input, cx);
        if self.volume.0 != mic {
            self.volume = (mic.clone(), open_volume(self.devices, mic));
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

    /// While the page is open and no call has the microphone open, the page opens it: for MONITOR
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
                let dsp = dsp_control(cx);
                let media = self.devices.config(
                    self.lobbies.read(cx).settings(),
                    self.level.clone(),
                    dsp,
                    self.dsp_levels.clone(),
                    self.monitor.clone(),
                );
                MicTest::monitored(media)
            }
        }
    }

    fn meter_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(list) = self.watch.take() {
            self.list = list;
            self.refresh(window, cx);
        }
        if let Some(fake) = &mut self.fake {
            for _ in 0..FAKE_RATE / 1000 * METER_FRAME.as_millis() as u32 {
                fake.push(FAKE_SAMPLE);
            }
        }
        let shown = self.level.read();
        let failure = self.test.as_ref().and_then(|(_, test)| test.failure());
        let rack = (self.in_call || self.monitoring_alone()).then(|| self.dsp_levels.read());
        let gain = self.volume.1.read();
        if drawn_differently(&shown, &self.shown)
            || failure != self.failure
            || RackModel::drawn(rack) != RackModel::drawn(self.rack_shown)
            || gain != self.gain_shown
        {
            self.shown = shown;
            self.failure = failure;
            self.rack_shown = rack;
            self.gain_shown = gain;
            cx.notify();
        }
    }

    /// Saved choices are written and heard from the call's next frame; GAIN goes to Windows and
    /// MONITOR to the call or the page's test, neither of them saved.
    fn change(&mut self, change: VoiceChange, cx: &mut Context<Self>) {
        match change {
            VoiceChange::Gain(level) => {
                self.volume.1.set(level);
                // Shown at once, so a held arrow steps on from the level asked, not the last read.
                self.gain_shown = MicGain::Level(level);
            }
            VoiceChange::Monitor(on) => {
                self.monitor.set_on(on);
                self.sync_test(cx);
            }
            VoiceChange::Rack(change) => self.save(|s| change.apply(s), cx),
            VoiceChange::Sounds(on) => self.save(|s| s.sounds = on, cx),
        }
        cx.notify();
    }

    fn save(&self, change: impl FnOnce(&mut Settings), cx: &mut App) {
        self.lobbies
            .update(cx, |l, cx| l.update_settings(change, cx));
        apply_dsp(&self.lobbies, cx);
    }
}

/// Fake media's microphone is no device, so it gets a volume that touches none.
fn open_volume(devices: VoiceDevices, mic: DeviceChoice) -> MicVolume {
    match devices {
        VoiceDevices::System => MicVolume::start(DeviceSetup::new(mic, devices.source())),
        VoiceDevices::Fake => MicVolume::fake(FAKE_GAIN),
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
    Rack(RackChange),
    /// The microphone's Windows input level, from 0 to 1.
    Gain(f32),
    Monitor(bool),
    Sounds(bool),
}

type OnChange = Rc<dyn Fn(VoiceChange, &mut Window, &mut App)>;

/// The section: the rack with the microphone's picker and meter on MIC and the speakers' picker on
/// OUT, then the UI sounds.
#[derive(IntoElement)]
pub struct VoiceSection {
    input: Entity<SelectState<Items>>,
    output: Entity<SelectState<Items>>,
    meter: Meter,
    rack: RackModel,
    layout: Option<RackLayout>,
    gain: MicGain,
    monitor: bool,
    sounds: bool,
    on_change: OnChange,
}

impl VoiceSection {
    pub fn new(
        input: &Entity<SelectState<Items>>,
        output: &Entity<SelectState<Items>>,
        meter: Meter,
        rack: RackModel,
        sounds: bool,
        on_change: impl Fn(VoiceChange, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            input: input.clone(),
            output: output.clone(),
            meter,
            rack,
            layout: None,
            gain: MicGain::Reading,
            monitor: false,
            sounds,
            on_change: Rc::new(on_change),
        }
    }

    /// The rack's layout whatever the window's width, for the gallery's frames.
    pub fn rack_layout(mut self, layout: RackLayout) -> Self {
        self.layout = Some(layout);
        self
    }

    /// What GAIN on MIC shows: the microphone's Windows input level.
    pub fn gain(mut self, gain: MicGain) -> Self {
        self.gain = gain;
        self
    }

    pub fn monitor(mut self, on: bool) -> Self {
        self.monitor = on;
        self
    }
}

/// A device picker as the rack's MIC and OUT hold it.
pub fn picker(
    state: &Entity<SelectState<Items>>,
    id: &'static str,
    label: &'static str,
) -> impl IntoElement + use<> {
    div()
        .id(id)
        .w_full()
        .child(
            Select::new(state)
                .small()
                .menu_width(u(PICKER_MENU_W))
                .accessibility_label(label),
        )
        .test_support()
}

impl RenderOnce for VoiceSection {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let layout = self
            .layout
            .unwrap_or_else(|| RackLayout::for_window(window));
        let rack = {
            let (rack, gain, monitor) = (
                self.on_change.clone(),
                self.on_change.clone(),
                self.on_change.clone(),
            );
            PatchRack::new(self.rack, layout)
                .devices(
                    picker(&self.input, "input-device", copy::DEVICE_INPUT),
                    picker(&self.output, "output-device", copy::DEVICE_OUTPUT),
                )
                .meter(self.meter)
                .gain(self.gain, move |level, window, cx| {
                    gain(VoiceChange::Gain(level), window, cx)
                })
                .monitor(self.monitor, move |on, window, cx| {
                    monitor(VoiceChange::Monitor(on), window, cx)
                })
                .on_change(move |change, window, cx| rack(VoiceChange::Rack(change), window, cx))
        };
        let sounds = {
            let on_change = self.on_change.clone();
            Switch::new("sounds")
                .checked(self.sounds)
                .accessibility_label(copy::SOUNDS)
                .on_click(move |on: &bool, window, cx| {
                    on_change(VoiceChange::Sounds(*on), window, cx)
                })
        };
        section(
            "Voice",
            div().child(div().pb_4().child(rack)).child(row(
                copy::SOUNDS,
                Some(copy::SOUNDS_DETAIL),
                sounds,
                cx,
            )),
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
            meter,
            RackModel::new(settings, self.rack_shown),
            settings.sounds,
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
