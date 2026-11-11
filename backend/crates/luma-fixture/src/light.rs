//! What light a cell can make: the additive emitters it has (from the
//! geometry's `emitter.colour`, FORMAT.md "Colour values") and the ones a
//! mode drives it with (from `control`, CONTROL.md §3 and §5). A cell emits
//! through an emitter only where both agree.
//!
//! Filters on a source (CMY, colour wheels, CCT) are not emitters: a white
//! lamp behind CMY flags has one `white` emitter, and the flags colour it.

use crate::control::{Attr, Resolved};
use crate::Built;

/// One additive emitter colour: the vocabulary `colour/<emitter>` drives and
/// `emitter.colour` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EmitterColour {
    Red,
    Green,
    Blue,
    White,
    Amber,
    Lime,
    Cyan,
    Indigo,
    Mint,
    Orange,
    Uv,
    WarmWhite,
    CoolWhite,
}

impl EmitterColour {
    pub const ALL: [Self; 13] = [
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::White,
        Self::Amber,
        Self::Lime,
        Self::Cyan,
        Self::Indigo,
        Self::Mint,
        Self::Orange,
        Self::Uv,
        Self::WarmWhite,
        Self::CoolWhite,
    ];

    /// The emitter a `colour/<emitter>` attribute drives; `None` for every
    /// other attribute, filters included.
    #[must_use]
    pub fn of(attr: Attr) -> Option<Self> {
        Some(match attr {
            Attr::Red => Self::Red,
            Attr::Green => Self::Green,
            Attr::Blue => Self::Blue,
            Attr::White => Self::White,
            Attr::Amber => Self::Amber,
            Attr::Lime => Self::Lime,
            Attr::Cyan => Self::Cyan,
            Attr::Indigo => Self::Indigo,
            Attr::Mint => Self::Mint,
            Attr::Orange => Self::Orange,
            Attr::Uv => Self::Uv,
            Attr::WarmWhite => Self::WarmWhite,
            Attr::CoolWhite => Self::CoolWhite,
            _ => return None,
        })
    }

    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "red" => Self::Red,
            "green" => Self::Green,
            "blue" => Self::Blue,
            "white" => Self::White,
            "amber" => Self::Amber,
            "lime" => Self::Lime,
            "cyan" => Self::Cyan,
            "indigo" => Self::Indigo,
            "mint" => Self::Mint,
            "orange" => Self::Orange,
            "uv" => Self::Uv,
            "warm_white" => Self::WarmWhite,
            "cool_white" => Self::CoolWhite,
            _ => return None,
        })
    }

    fn letter(c: char) -> Option<Self> {
        Some(match c {
            'r' => Self::Red,
            'g' => Self::Green,
            'b' => Self::Blue,
            'a' => Self::Amber,
            'w' => Self::White,
            'l' => Self::Lime,
            'c' => Self::Cyan,
            'i' => Self::Indigo,
            'm' => Self::Mint,
            _ => return None,
        })
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// A set of [`EmitterColour`]s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EmitterSet(u16);

impl std::fmt::Debug for EmitterSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl EmitterSet {
    pub const EMPTY: Self = Self(0);

    #[must_use]
    pub fn contains(self, e: EmitterColour) -> bool {
        self.0 & e.bit() != 0
    }

    pub fn insert(&mut self, e: EmitterColour) {
        self.0 |= e.bit();
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Whether a cell with these dies lights when a `colour/<channel>`
    /// channel drives it. Exact, except across whites: a `white` channel
    /// drives any white die (a fixture's "W" is often warm), and a
    /// `warm_white` or `cool_white` channel drives a die the geometry only
    /// calls `white`.
    #[must_use]
    pub fn lit_by(self, channel: EmitterColour) -> bool {
        use EmitterColour::{CoolWhite, WarmWhite, White};
        self.contains(channel)
            || match channel {
                White => self.contains(WarmWhite) || self.contains(CoolWhite),
                WarmWhite | CoolWhite => self.contains(White),
                _ => false,
            }
    }

    /// The channels in `self` that a cell with `dies` does not light.
    #[must_use]
    pub fn unlit_on(self, dies: Self) -> Self {
        self.iter().filter(|&e| !dies.lit_by(e)).collect()
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = EmitterColour> {
        EmitterColour::ALL
            .into_iter()
            .filter(move |&e| self.contains(e))
    }

    /// The emitters an `emitter.colour` value names: one emitter's name
    /// (`white`, `warm_white`, `uv`), `ww_cw`, `cmy` (a white lamp behind
    /// filters), or mixing letters in `r g b a w l c i m` with an optional
    /// `+uv`, `+ww_cw`, `+warm_white` or `+cool_white`. `None` for anything
    /// else.
    #[must_use]
    pub fn parse(colour: &str) -> Option<Self> {
        let mut set = Self::EMPTY;
        if let Some(e) = EmitterColour::named(colour) {
            set.insert(e);
            return Some(set);
        }
        let (letters, extra) = match colour.split_once('+') {
            Some((l, x)) => (l, Some(x)),
            None => (colour, None),
        };
        match letters {
            "cmy" if extra.is_none() => set.insert(EmitterColour::White),
            "ww_cw" if extra.is_none() => {
                set.insert(EmitterColour::WarmWhite);
                set.insert(EmitterColour::CoolWhite);
            }
            _ => {
                if letters.is_empty() {
                    return None;
                }
                for c in letters.chars() {
                    set.insert(EmitterColour::letter(c)?);
                }
            }
        }
        match extra {
            None => {}
            Some("ww_cw") => {
                set.insert(EmitterColour::WarmWhite);
                set.insert(EmitterColour::CoolWhite);
            }
            Some(x) => set.insert(EmitterColour::named(x)?),
        }
        Some(set)
    }
}

impl FromIterator<EmitterColour> for EmitterSet {
    fn from_iter<I: IntoIterator<Item = EmitterColour>>(iter: I) -> Self {
        let mut set = Self::EMPTY;
        for e in iter {
            set.insert(e);
        }
        set
    }
}

/// What the geometry says a cell has, or why it cannot say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellEmitters {
    /// Every emitter of the cell names a colour; the union of them.
    Known(EmitterSet),
    /// Some emitter of the cell names no colour, or the cell draws none.
    Unknown,
    /// An emitter names a colour outside the vocabulary.
    Invalid,
}

impl Built {
    /// The emitters the geometry gives cell `index`.
    #[must_use]
    pub fn cell_emitters(&self, index: usize) -> CellEmitters {
        let Some(cell) = self.cells.get(index) else {
            return CellEmitters::Unknown;
        };
        if cell.emitters.is_empty() {
            return CellEmitters::Unknown;
        }
        let mut set = EmitterSet::EMPTY;
        for e in &cell.emitters {
            let Some(colour) = e.colour.as_deref() else {
                return CellEmitters::Unknown;
            };
            let Some(s) = EmitterSet::parse(colour) else {
                return CellEmitters::Invalid;
            };
            set = set.union(s);
        }
        CellEmitters::Known(set)
    }
}

/// Where a mode and the geometry disagree about a cell's emitters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// A channel drives `emitters` on `cell`, whose geometry has none of
    /// them.
    Undrawn { cell: usize, emitters: EmitterSet },
    /// A fixture-level channel drives `emitters` that no cell has.
    Nowhere { emitters: EmitterSet },
    /// Cell `cell`'s geometry names a colour outside the vocabulary.
    InvalidColour { cell: usize },
}

impl Resolved {
    /// The emitters each of `cells` cells is driven with in this mode: the
    /// cell-scoped `colour/<emitter>` params covering it, and every
    /// fixture-level one. A fixture of one cell (or none) counts as one.
    #[must_use]
    pub fn driven_emitters(&self, cells: usize) -> Vec<EmitterSet> {
        let mut driven = vec![EmitterSet::EMPTY; cells.max(1)];
        for p in &self.params {
            let Some(e) = EmitterColour::of(p.attr) else {
                continue;
            };
            match p.cell {
                None => driven.iter_mut().for_each(|d| d.insert(e)),
                Some(cell) => {
                    let [a, b] = cell.span();
                    for d in driven.iter_mut().take(b as usize + 1).skip(a as usize) {
                        d.insert(e);
                    }
                }
            }
        }
        driven
    }

    /// Every place this mode drives an emitter the geometry does not draw. An
    /// emulated channel is made from the other dies, so it needs none.
    #[must_use]
    pub fn mismatches(&self, built: &Built) -> Vec<Mismatch> {
        let cells = built.cells.len().max(1);
        let known: Vec<CellEmitters> = (0..cells).map(|i| built.cell_emitters(i)).collect();
        let mut out = Vec::new();
        let mut anywhere = EmitterSet::EMPTY;
        let mut all_known = true;
        for (i, k) in known.iter().enumerate() {
            match *k {
                CellEmitters::Known(s) => anywhere = anywhere.union(s),
                CellEmitters::Unknown => all_known = false,
                CellEmitters::Invalid => {
                    all_known = false;
                    out.push(Mismatch::InvalidColour { cell: i });
                }
            }
        }
        let mut fixture_level = EmitterSet::EMPTY;
        let mut scoped = vec![EmitterSet::EMPTY; cells];
        for p in self.params.iter().filter(|p| !p.emulated) {
            let Some(e) = EmitterColour::of(p.attr) else {
                continue;
            };
            match p.cell {
                None => fixture_level.insert(e),
                Some(cell) => {
                    let [a, b] = cell.span();
                    for s in scoped.iter_mut().take(b as usize + 1).skip(a as usize) {
                        s.insert(e);
                    }
                }
            }
        }
        for (i, s) in scoped.into_iter().enumerate() {
            if let CellEmitters::Known(has) = known[i] {
                let missing = s.unlit_on(has);
                if !missing.is_empty() {
                    out.push(Mismatch::Undrawn {
                        cell: i,
                        emitters: missing,
                    });
                }
            }
        }
        if all_known {
            let missing = fixture_level.unlit_on(anywhere);
            if !missing.is_empty() {
                out.push(Mismatch::Nowhere { emitters: missing });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use EmitterColour::*;

    fn set(es: &[EmitterColour]) -> EmitterSet {
        es.iter().copied().collect()
    }

    #[test]
    fn parses_every_form_the_format_names() {
        assert_eq!(EmitterSet::parse("white"), Some(set(&[White])));
        assert_eq!(
            EmitterSet::parse("rgbw"),
            Some(set(&[Red, Green, Blue, White]))
        );
        assert_eq!(
            EmitterSet::parse("rgb+warm_white"),
            Some(set(&[Red, Green, Blue, WarmWhite]))
        );
        assert_eq!(
            EmitterSet::parse("rgbaw+uv"),
            Some(set(&[Red, Green, Blue, Amber, White, Uv]))
        );
        assert_eq!(
            EmitterSet::parse("a+ww_cw"),
            Some(set(&[Amber, WarmWhite, CoolWhite]))
        );
        assert_eq!(
            EmitterSet::parse("ww_cw"),
            Some(set(&[WarmWhite, CoolWhite]))
        );
        assert_eq!(EmitterSet::parse("cmy"), Some(set(&[White])));
        assert_eq!(
            EmitterSet::parse("rgbaim"),
            Some(set(&[Red, Green, Blue, Amber, Indigo, Mint]))
        );
        for bad in ["", "rgbx", "rgb+", "rgb+teal", "RGB", "cmy+uv"] {
            assert_eq!(EmitterSet::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_white_channel_lights_any_white_die_and_nothing_else() {
        let warm = set(&[Red, Green, Blue, WarmWhite]);
        assert!(warm.lit_by(White));
        assert!(warm.lit_by(WarmWhite));
        assert!(!warm.lit_by(CoolWhite));
        assert!(!warm.lit_by(Amber));
        assert!(set(&[White]).lit_by(CoolWhite));
        assert!(!set(&[Red, Green, Blue]).lit_by(White));
        assert_eq!(set(&[Red, White, Uv]).unlit_on(warm), set(&[Uv]));
    }
}
