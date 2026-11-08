//! When a share's controls show, as a video player's do: they fade in when the pointer moves over
//! the picture, fade out [`CONTROLS_IDLE`] after it last moved, and go at once when it leaves the
//! picture. While the pointer is on the controls themselves, or one of them has the keyboard
//! focus, they stay. Pure: the view feeds it pointer events and the clock.

use std::time::{Duration, Instant};

/// How long the pointer may rest on the picture before the controls fade out.
pub const CONTROLS_IDLE: Duration = Duration::from_secs(2);
pub const FADE_IN: Duration = Duration::from_millis(150);
pub const FADE_OUT: Duration = Duration::from_millis(300);

/// Where the pointer was last seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pointer {
    #[default]
    Away,
    Picture,
    Controls,
}

/// What the controls look like at a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Faded out for good: drawn clear, and covered so nothing under them takes a click.
    Hidden,
    /// Fading in or out. `turn` names the fade, so each one starts its own animation.
    Fading {
        shown: bool,
        turn: u32,
    },
    Shown,
}

/// The last change of [`Controls::shown`], for the fade.
#[derive(Debug, Clone, Copy, Default)]
struct Turn {
    shown: bool,
    at: Option<Instant>,
    count: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Controls {
    pointer: Pointer,
    moved: Option<Instant>,
    turn: Turn,
}

impl Controls {
    pub fn moved(&mut self, at: Pointer, now: Instant) {
        self.pointer = at;
        self.moved = Some(now);
    }

    /// The pointer left `from`. Leaving one place for the next reports the new place first or
    /// last, so only the place it is still at counts.
    pub fn left(&mut self, from: Pointer) {
        if self.pointer == from {
            self.pointer = Pointer::Away;
        }
    }

    /// `held`: one of the controls has the keyboard focus.
    pub fn shown(&self, now: Instant, held: bool) -> bool {
        held || match self.pointer {
            Pointer::Away => false,
            Pointer::Controls => true,
            Pointer::Picture => self.idle_for(now) < CONTROLS_IDLE,
        }
    }

    /// How long until the pointer's rest on the picture hides the controls, if it will.
    pub fn hides_in(&self, now: Instant) -> Option<Duration> {
        (self.pointer == Pointer::Picture)
            .then(|| CONTROLS_IDLE.saturating_sub(self.idle_for(now)))
            .filter(|left| !left.is_zero())
    }

    fn idle_for(&self, now: Instant) -> Duration {
        self.moved
            .map_or(Duration::MAX, |at| now.saturating_duration_since(at))
    }

    /// What to draw at `now`. Called as the view renders, so each change of [`Self::shown`] seen
    /// there starts one fade.
    pub fn look(&mut self, now: Instant, held: bool) -> Look {
        let shown = self.shown(now, held);
        if shown != self.turn.shown {
            self.turn = Turn {
                shown,
                at: Some(now),
                count: self.turn.count + 1,
            };
        }
        let fade = if shown { FADE_IN } else { FADE_OUT };
        match self.turn.at {
            Some(at) if now.saturating_duration_since(at) < fade => Look::Fading {
                shown,
                turn: self.turn.count,
            },
            _ if shown => Look::Shown,
            _ => Look::Hidden,
        }
    }
}
