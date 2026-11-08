//! `Lobbies`: the Lobbies on this device as an entity. It holds what `state.json` says, bumps a
//! generation on every real change and writes the file [`SAVE_DEBOUNCE`] after the last change of
//! a burst, off the UI thread. The Quit flow writes whatever is still due ([`Lobbies::flush_now`]).
//! A failed write is an Issue for the Shell to show.

use std::{collections::HashSet, path::PathBuf, time::Duration};

use gpui_kit::{Context, EventEmitter, Hsla, SharedString, Task};
use lobby_client::{
    Issue, Lobby, LobbyOrigin, PersistError, Persisted, SavedWindow, Settings, StateFile, Upserted,
};
use lobby_protocol::{LobbyInfo, MessageId};
use tracing::{info, warn};

use crate::components::{
    SwitcherCurrent, SwitcherEntry, mark_color, mark_slots, shown_address, slot_color,
};

pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LobbiesEvent {
    SaveFailed(Issue),
    SettingsChanged,
}

pub struct Lobbies {
    file: StateFile,
    state: Persisted,
    generation: u64,
    save_timer: Option<Task<()>>,
    saving: Option<Task<()>>,
    /// Never written: the token store is the truth; this only lets the switcher say which Lobbies
    /// will ask for a Sign In.
    signed_in: HashSet<LobbyOrigin>,
    kept_file: Option<PathBuf>,
}

impl EventEmitter<LobbiesEvent> for Lobbies {}

impl Lobbies {
    pub fn new(file: StateFile, state: Persisted) -> Self {
        Self {
            file,
            state,
            generation: 0,
            save_timer: None,
            saving: None,
            signed_in: HashSet::new(),
            kept_file: None,
        }
    }

    pub fn with_startup(mut self, signed_in: HashSet<LobbyOrigin>, kept: Option<PathBuf>) -> Self {
        self.signed_in = signed_in;
        self.kept_file = kept;
        self
    }

    pub fn kept_file(&self) -> Option<&PathBuf> {
        self.kept_file.as_ref()
    }

    pub fn is_signed_in(&self, origin: &LobbyOrigin) -> bool {
        self.signed_in.contains(origin)
    }

    pub fn set_signed_in(&mut self, origin: &LobbyOrigin, signed_in: bool, cx: &mut Context<Self>) {
        let changed = if signed_in {
            self.signed_in.insert(origin.clone())
        } else {
            self.signed_in.remove(origin)
        };
        if changed {
            cx.notify();
        }
    }

    pub fn color_of(&self, origin: &LobbyOrigin) -> Hsla {
        let lobbies = self.list();
        match lobbies.iter().position(|l| &l.origin == origin) {
            Some(ix) => slot_color(mark_slots(lobbies.iter().map(|l| l.origin.as_str()))[ix]),
            None => mark_color(origin.as_str()),
        }
    }

    /// The name the Lobby gave itself just now (`live`) when it has one, else the one saved.
    pub fn name_or<'a>(&'a self, origin: &LobbyOrigin, live: Option<&'a str>) -> Option<&'a str> {
        let named = |n: &&str| !n.trim().is_empty();
        live.filter(named)
            .or_else(|| self.get(origin).map(|l| l.name.as_str()).filter(named))
    }

    /// [`Self::name_or`], or the address when the Lobby never gave a name.
    pub fn display_name(&self, origin: &LobbyOrigin, live: Option<&str>) -> String {
        self.name_or(origin, live)
            .map_or_else(|| shown_address(origin).to_string(), str::to_owned)
    }

    /// The switcher's rows. With no current Lobby (the URL step) the last active one gets a muted
    /// dot, since its address is the one in the field.
    pub fn switcher_entries(&self, current: Option<&LobbyOrigin>) -> Vec<SwitcherEntry> {
        let lobbies = self.list();
        let slots = mark_slots(lobbies.iter().map(|l| l.origin.as_str()));
        let last = self.state.last_active.as_ref();
        lobbies
            .iter()
            .zip(slots)
            .map(|(l, slot)| SwitcherEntry {
                origin: l.origin.clone(),
                name: l.name.clone().into(),
                icon: l.icon.clone(),
                color: slot_color(slot),
                current: Some(&l.origin) == current,
                last_used: current.is_none() && Some(&l.origin) == last,
                signed_in: self.signed_in.contains(&l.origin),
            })
            .collect()
    }

    pub fn switcher_current(&self, origin: &LobbyOrigin, name: Option<&str>) -> SwitcherCurrent {
        let name: SharedString = self
            .name_or(origin, name)
            .unwrap_or_default()
            .to_owned()
            .into();
        SwitcherCurrent {
            name,
            origin: origin.clone(),
            icon: self.get(origin).and_then(|l| l.icon.clone()),
            color: self.color_of(origin),
        }
    }

    pub fn persisted(&self) -> &Persisted {
        &self.state
    }

    pub fn list(&self) -> &[Lobby] {
        &self.state.lobbies
    }

    pub fn get(&self, origin: &LobbyOrigin) -> Option<&Lobby> {
        self.state.lobby(origin)
    }

    pub fn upsert(
        &mut self,
        origin: &LobbyOrigin,
        info: &LobbyInfo,
        cx: &mut Context<Self>,
    ) -> Upserted {
        let upserted = self.state.upsert(origin, info);
        match upserted {
            Upserted::Same => {}
            Upserted::Reset => {
                info!(lobby = %origin, instance = %info.instance_id.as_str(), "the Lobby was reset");
                self.changed(cx);
            }
            Upserted::Added | Upserted::Updated => {
                info!(lobby = %origin, name = %info.name, ?upserted, "lobby saved");
                self.changed(cx);
            }
        }
        upserted
    }

    /// A Lobby the Member left: gone from the switcher and from `state.json`, never restored.
    pub fn remove(&mut self, origin: &LobbyOrigin, cx: &mut Context<Self>) {
        self.signed_in.remove(origin);
        if self.state.remove(origin) {
            info!(lobby = %origin, "lobby removed");
            self.changed(cx);
        }
    }

    pub fn set_last_active(&mut self, origin: &LobbyOrigin, cx: &mut Context<Self>) {
        if self.state.last_active.as_ref() != Some(origin) {
            self.state.last_active = Some(origin.clone());
            self.changed(cx);
        }
    }

    /// What was typed in the Lobby's composer and not sent.
    pub fn unsent(&self, origin: &LobbyOrigin) -> Option<&str> {
        self.state.unsent.get(origin).map(String::as_str)
    }

    /// The composer's text, kept per Lobby so a switch or a restart does not lose it. Empty text
    /// clears it.
    pub fn set_unsent(&mut self, origin: &LobbyOrigin, text: &str, cx: &mut Context<Self>) {
        let changed = if text.is_empty() {
            self.state.unsent.remove(origin).is_some()
        } else if self.unsent(origin) == Some(text) {
            false
        } else {
            self.state.unsent.insert(origin.clone(), text.to_owned());
            true
        };
        if changed {
            self.changed(cx);
        }
    }

    /// The newest Message the Member has had in front of them in this Lobby.
    pub fn seen(&self, origin: &LobbyOrigin) -> Option<MessageId> {
        self.state.seen.get(origin).copied()
    }

    /// Only moves forward: reading an older page, or a Lobby that reloads, never takes it back.
    pub fn set_seen(&mut self, origin: &LobbyOrigin, newest: MessageId, cx: &mut Context<Self>) {
        if self.seen(origin).is_none_or(|seen| seen < newest) {
            self.state.seen.insert(origin.clone(), newest);
            self.changed(cx);
        }
    }

    /// What one Member saw is not what the next person to sign in on this device saw.
    pub fn forget_seen(&mut self, origin: &LobbyOrigin, cx: &mut Context<Self>) {
        if self.state.seen.remove(origin).is_some() {
            self.changed(cx);
        }
    }

    pub fn sidebar_width(&self) -> Option<f32> {
        self.state.sidebar_width.map(|w| w as f32)
    }

    /// Saved in whole pixels, so a drag's sub-pixel moves write nothing new.
    pub fn set_sidebar_width(&mut self, width: Option<f32>, cx: &mut Context<Self>) {
        let width = width.map(|w| w.round().max(0.) as u32);
        if self.state.sidebar_width != width {
            self.state.sidebar_width = width;
            self.changed(cx);
        }
    }

    pub fn sidebar_collapsed(&self) -> bool {
        self.state.sidebar_collapsed
    }

    pub fn set_sidebar_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        if self.state.sidebar_collapsed != collapsed {
            self.state.sidebar_collapsed = collapsed;
            self.changed(cx);
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.state.settings
    }

    /// The one way a setting changes: what the Shell shows follows the event, the file follows
    /// the debounce.
    pub fn update_settings(&mut self, edit: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut settings = self.state.settings.clone();
        edit(&mut settings);
        settings.normalize();
        if settings != self.state.settings {
            self.state.settings = settings;
            self.changed(cx);
            cx.emit(LobbiesEvent::SettingsChanged);
        }
    }

    pub fn set_window(&mut self, window: SavedWindow, cx: &mut Context<Self>) {
        if self.state.window.as_ref() != Some(&window) {
            self.state.window = Some(window);
            // No view shows where the window is, so a move redraws nothing.
            self.save_soon(cx);
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        self.save_soon(cx);
    }

    /// The write waits for [`SAVE_DEBOUNCE`] without a change, so a window dragged across the
    /// screen is written once, where it stops. A new timer drops the one before it.
    fn save_soon(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let timer = cx.background_executor().timer(SAVE_DEBOUNCE);
        self.save_timer = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |lobbies, cx| lobbies.save_now(cx));
        }));
    }

    /// Runs on the background executor (a file and an fsync). A newer write, or the Quit flow's,
    /// supersedes it.
    fn save_now(&mut self, cx: &mut Context<Self>) {
        self.save_timer = None;
        let (file, generation, state) = (self.file.clone(), self.generation, self.state.clone());
        let write = cx
            .background_executor()
            .spawn(async move { file.write(generation, &state) });
        self.saving = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = write.await {
                warn!(error = %e, "cannot save the Lobbies");
                let _ = this.update(cx, |_, cx| cx.emit(LobbiesEvent::SaveFailed(e.issue())));
            }
        }));
    }

    pub fn flush_now(&mut self) -> Result<(), PersistError> {
        self.save_timer = None;
        if self.generation == 0 {
            return Ok(());
        }
        self.file.write(self.generation, &self.state).map(|_| ())
    }
}
