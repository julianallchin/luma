//! How a button plays: what a press does, and the envelope its value
//! follows.
//!
//! Every `button` node of a graph plays this way, a Perform pattern's pad
//! among them ([`crate::ClipGraph::pad`]). A [`Press`] says whether a press
//! turns the button on until the next press ([`PressMode::Toggle`]) or only
//! while held ([`PressMode::Flash`]), and an [`Adsr`] shapes the value
//! through the press: it rises over the attack, falls to the sustain over the
//! decay, holds while on, and falls to 0 over the release. A soft wash is a
//! slow attack and release at sustain 1; a hit is a fast attack at sustain 0,
//! punchy, which fires once however long it is held.
//!
//! A press takes effect now, or on the set's next beat or bar ([`Start`]),
//! turning on and turning off alike, so busking lands on the music.
//!
//! The value is a pure function of when the button goes on and off and of
//! now ([`Adsr::value`]), so a host keeps only those two times per button,
//! and anything shown a [`Button`] can play its envelope on, frame by frame.
//! A press waiting for its beat is a time still to come.

use serde::{Deserialize, Serialize};

/// What a press does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressMode {
    /// On at a press, off at the next.
    #[default]
    Toggle,
    /// On while held.
    Flash,
}

/// When a press takes effect, turning on and turning off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    #[default]
    Now,
    /// On the set's next beat.
    Beat,
    /// On the set's next bar.
    Bar,
}

/// How a stage of an envelope moves from where it starts to where it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// Straight.
    Linear,
    /// Eased in and out.
    #[default]
    Smooth,
    /// Fast at first, then settling: a hit.
    Punchy,
    /// No ramp: the stage jumps to where it ends.
    Snap,
}

impl Shape {
    /// Progress `t` 0–1 through a stage, shaped.
    pub fn eval(self, t: f64) -> f64 {
        let t = t.clamp(0., 1.);
        match self {
            Self::Linear => t,
            Self::Smooth => t * t * (3. - 2. * t),
            Self::Punchy => 1. - (1. - t).powi(3),
            Self::Snap => 1.,
        }
    }
}

/// What a stage's length is counted in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeUnit {
    /// Beats of the tempo the set plays at, so a release follows the music.
    #[default]
    Beats,
    Seconds,
}

impl TimeUnit {
    /// `length` of this unit in seconds at `bpm`.
    pub fn seconds(self, length: f64, bpm: f64) -> f64 {
        match self {
            TimeUnit::Seconds => length.max(0.),
            TimeUnit::Beats => length.max(0.) * 60. / bpm.max(1.),
        }
    }
}

/// An attack, decay, sustain and release, in `unit`, shaped by `shape`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Adsr {
    pub attack: f64,
    pub decay: f64,
    /// The level held while on, 0–1.
    pub sustain: f64,
    pub release: f64,
    pub unit: TimeUnit,
    pub shape: Shape,
}

impl Default for Adsr {
    /// On and off at once: a plain switch.
    fn default() -> Self {
        Self {
            attack: 0.,
            decay: 0.,
            sustain: 1.,
            release: 0.,
            unit: TimeUnit::Beats,
            shape: Shape::Smooth,
        }
    }
}

/// When a button goes on and, if it has been let go, off: seconds before
/// now, less than 0 while still to come.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gate {
    pub on_for: f64,
    /// Seconds since it went off, `None` while it is on.
    pub off_for: Option<f64>,
}

impl Gate {
    /// Whether it has gone on and not yet off.
    pub fn on(&self) -> bool {
        self.on_for >= 0. && self.off_for.is_none_or(|off| off < 0.)
    }

    /// Whether it waits to go on or off.
    pub fn waiting(&self) -> bool {
        self.on_for < 0. || self.off_for.is_some_and(|off| off < 0.)
    }
}

impl Adsr {
    /// A stage's `length` in seconds at `bpm`.
    fn seconds(&self, length: f64, bpm: f64) -> f64 {
        self.unit.seconds(length, bpm)
    }

    /// The value `gate` puts out now, 0–1, at `bpm` (for lengths in beats).
    pub fn value(&self, gate: Gate, bpm: f64) -> f64 {
        let seconds = |length| self.seconds(length, bpm);
        let stage = |elapsed: f64, length: f64| {
            if length <= 0. {
                1.
            } else {
                self.shape.eval(elapsed / length)
            }
        };
        let (attack, decay, release) = (
            seconds(self.attack),
            seconds(self.decay),
            seconds(self.release),
        );
        let sustain = self.sustain.clamp(0., 1.);
        // Where the envelope was at a time `on` seconds after the press.
        let held = |on: f64| {
            if on < attack {
                stage(on, attack)
            } else if on < attack + decay {
                1. - (1. - sustain) * stage(on - attack, decay)
            } else {
                sustain
            }
        };
        match gate.off_for {
            _ if gate.on_for < 0. => 0.,
            Some(off) if off >= 0. => {
                let from = held((gate.on_for - off).max(0.));
                from * (1. - stage(off, release))
            }
            _ => held(gate.on_for),
        }
    }

    /// Whether the value is still changing: on and rising or decaying, off
    /// and releasing, or waiting to do either.
    pub fn moving(&self, gate: Gate, bpm: f64) -> bool {
        let seconds = |length| self.seconds(length, bpm);
        gate.waiting()
            || match gate.off_for {
                None => gate.on_for < seconds(self.attack) + seconds(self.decay),
                Some(off) => off < seconds(self.release),
            }
    }
}

/// A button's press, when it takes effect, and its envelope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Press {
    pub mode: PressMode,
    pub start: Start,
    pub envelope: Adsr,
}

/// A button as it stands: how it plays, and its gate, `None` until first
/// pressed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Button {
    pub press: Press,
    pub gate: Option<Gate>,
}

impl Button {
    /// Pressed and held, or toggled on. A button releasing is off, though
    /// its envelope may still sound; one waiting for its beat is as it was.
    pub fn on(&self) -> bool {
        self.gate.is_some_and(|gate| gate.on())
    }

    /// Whether a press waits for its beat or bar to turn it on or off.
    pub fn waiting(&self) -> bool {
        self.gate.is_some_and(|gate| gate.waiting())
    }

    /// Its value, 0–1, at `bpm`.
    pub fn value(&self, bpm: f64) -> f64 {
        self.gate
            .map_or(0., |gate| self.press.envelope.value(gate, bpm))
    }

    /// Whether its value is still changing.
    pub fn moving(&self, bpm: f64) -> bool {
        self.gate
            .is_some_and(|gate| self.press.envelope.moving(gate, bpm))
    }

    /// As it stands `seconds` later, nothing pressed meanwhile.
    pub fn later(self, seconds: f64) -> Self {
        Self {
            gate: self.gate.map(|gate| Gate {
                on_for: gate.on_for + seconds,
                off_for: gate.off_for.map(|off| off + seconds),
            }),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BPM: f64 = 120.;

    fn on(on_for: f64) -> Gate {
        Gate {
            on_for,
            off_for: None,
        }
    }

    #[test]
    fn a_plain_switch_is_on_while_on() {
        let adsr = Adsr::default();
        assert_eq!(adsr.value(on(0.), BPM), 1.);
        let off = Gate {
            on_for: 3.,
            off_for: Some(0.),
        };
        assert_eq!(adsr.value(off, BPM), 0.);
    }

    #[test]
    fn a_wash_fades_in_and_out_over_beats() {
        let adsr = Adsr {
            attack: 2.,
            release: 4.,
            shape: Shape::Linear,
            ..Adsr::default()
        };
        // Two beats at 120 BPM is one second.
        assert!((adsr.value(on(0.5), BPM) - 0.5).abs() < 1e-9);
        assert_eq!(adsr.value(on(1.5), BPM), 1.);
        let releasing = Gate {
            on_for: 3.,
            off_for: Some(1.),
        };
        assert!(
            (adsr.value(releasing, BPM) - 0.5).abs() < 1e-9,
            "half of a two-second release"
        );
    }

    #[test]
    fn a_hit_fires_once_however_long_it_is_held() {
        let hit = Adsr {
            attack: 0.,
            decay: 0.25,
            sustain: 0.,
            release: 0.,
            unit: TimeUnit::Seconds,
            shape: Shape::Punchy,
        };
        assert_eq!(hit.value(on(0.), BPM), 1.);
        assert!(hit.value(on(0.1), BPM) < 1.);
        assert!(hit.moving(on(0.1), BPM));
        assert_eq!(hit.value(on(2.), BPM), 0.);
        assert!(!hit.moving(on(2.), BPM), "settled at sustain");
    }

    #[test]
    fn a_press_still_to_come_waits_dark_and_a_release_still_to_come_holds() {
        let adsr = Adsr {
            attack: 1.,
            unit: TimeUnit::Seconds,
            shape: Shape::Linear,
            ..Adsr::default()
        };
        let waiting = Gate {
            on_for: -0.5,
            off_for: None,
        };
        assert_eq!(adsr.value(waiting, BPM), 0.);
        assert!(adsr.moving(waiting, BPM), "it will light");
        assert!(!waiting.on() && waiting.waiting());
        let button = Button {
            press: Press {
                envelope: adsr,
                ..Press::default()
            },
            gate: Some(waiting),
        };
        let lit = button.later(1.);
        assert!(lit.on() && !lit.waiting(), "on at its beat");
        assert!((lit.value(BPM) - 0.5).abs() < 1e-9, "halfway up its attack");

        let letting_go = Gate {
            on_for: 2.,
            off_for: Some(-0.5),
        };
        assert_eq!(adsr.value(letting_go, BPM), 1., "on until its beat");
        assert!(letting_go.on() && letting_go.waiting());
    }

    #[test]
    fn a_release_starts_from_wherever_the_attack_got_to() {
        let adsr = Adsr {
            attack: 1.,
            release: 1.,
            unit: TimeUnit::Seconds,
            shape: Shape::Linear,
            ..Adsr::default()
        };
        // Let go halfway up: the release falls from 0.5, not from 1.
        let early = Gate {
            on_for: 0.75,
            off_for: Some(0.25),
        };
        assert!((adsr.value(early, BPM) - 0.375).abs() < 1e-9);
    }
}
