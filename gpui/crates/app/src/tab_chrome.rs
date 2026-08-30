//! Pure state behind the workspace tab strip's menu and close/reflow motion.
//!
//! [`crate::tabs::Tabs`] remains the authority for open targets, selection and
//! teardown. This module remembers only what chrome needs after that logical
//! mutation has happened: where surviving chips were, which chip is fading
//! out, and whether the pointer is still inside the one short-lived region
//! allowed to forward a consecutive close.

use std::time::{Duration, Instant};

use crate::tabs::Target;
use crate::Luma;
use luma_ui::motion::{self, TAB_SLIDE};

pub(crate) const CHIP_MAX_WIDTH: f32 = 148.0;
pub(crate) const CHIP_GAP: f32 = 4.0;

#[derive(Debug, Clone)]
pub(crate) struct TabDescriptor {
    pub(crate) target: Target,
    pub(crate) title: String,
}

#[derive(Debug, Clone)]
pub(crate) struct LiveChipFrame {
    pub(crate) target: Target,
    pub(crate) title: String,
    /// Relative paint offset from the chip's final flex slot.
    pub(crate) x_offset: f32,
    pub(crate) width: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct ExitChipFrame {
    pub(crate) target: Target,
    pub(crate) title: String,
    /// Absolute position inside the strip.
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) opacity: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct TabStripFrame {
    pub(crate) live: Vec<LiveChipFrame>,
    /// The live gap contracts only after chip widths reach zero, preserving
    /// the fixed control group at the edge of an extremely narrow strip.
    pub(crate) gap: f32,
    /// Relative paint offset for the `+` and collapse-control group.
    pub(crate) controls_x_offset: f32,
    /// Where the control group *is* this frame, in strip space — the anchor
    /// the `+` menu hangs from. Published rather than re-derived at the call
    /// site from a first-chip width, which is the same number only while every
    /// chip is the same width.
    pub(crate) controls_x: f32,
    pub(crate) animating: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TabTransitionFrame {
    pub(crate) exits: Vec<ExitChipFrame>,
    pub(crate) stable_close: Option<PointerRegion>,
    pub(crate) animating: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PointerRegion {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[derive(Debug, Clone)]
struct LiveChip {
    target: Target,
    title: String,
    x: ScalarMotion,
    width: f32,
}

#[derive(Debug, Clone)]
struct ExitChip {
    target: Target,
    title: String,
    x: f32,
    y: f32,
    width: f32,
    started: Instant,
}

#[derive(Debug, Clone, Copy)]
struct StableClose {
    /// The logical index the closed tab released. Its successor occupies this
    /// index, which is why forwarding here closes the expected next tab.
    index: usize,
    region: PointerRegion,
    expires: Instant,
    /// Once the pointer leaves, re-entry never arms this token again.
    pointer_inside: bool,
}

#[derive(Debug, Clone, Copy)]
struct ScalarMotion {
    from: f32,
    target: f32,
    started: Instant,
}

impl ScalarMotion {
    fn resting(value: f32, now: Instant) -> Self {
        Self {
            from: value,
            target: value,
            started: now,
        }
    }

    fn value(self, now: Instant) -> f32 {
        motion::lerp(self.from, self.target, progress(self.started, now))
    }

    fn retarget(&mut self, target: f32, now: Instant) {
        if (self.target - target).abs() <= f32::EPSILON {
            return;
        }
        self.from = self.value(now);
        self.target = target;
        self.started = now;
    }

    fn moving(self, now: Instant) -> bool {
        (self.value(now) - self.target).abs() > 0.5
    }
}

#[derive(Debug)]
pub(crate) struct TabChrome {
    pub(crate) menu_open: bool,
    /// Window-space origin of the one live strip owner this frame.
    strip_origin: (f32, f32),
    live: Vec<LiveChip>,
    exits: Vec<ExitChip>,
    controls_x: Option<ScalarMotion>,
    stable_close: Option<StableClose>,
}

impl Default for TabChrome {
    fn default() -> Self {
        Self {
            menu_open: false,
            strip_origin: (0.0, 0.0),
            live: Vec::new(),
            exits: Vec::new(),
            controls_x: None,
            stable_close: None,
        }
    }
}

impl TabChrome {
    pub(crate) fn set_strip_origin(&mut self, x: f32, y: f32) {
        self.strip_origin = (x, y);
    }

    pub(crate) fn toggle_menu(&mut self) {
        self.menu_open = !self.menu_open;
    }

    pub(crate) fn dismiss_menu(&mut self) -> bool {
        std::mem::take(&mut self.menu_open)
    }

    /// Capture the visual chip before its logical tab is removed.
    ///
    /// `pointer_region` is present only for a pointer-triggered close. It is
    /// the exact original close affordance and lasts for one transition; a
    /// hover-leave permanently invalidates it, so an unrelated later re-entry
    /// can never close the tab that happens to occupy the same coordinates.
    pub(crate) fn begin_close(
        &mut self,
        target: &Target,
        pointer_region: Option<PointerRegion>,
        reduced_motion: bool,
        now: Instant,
    ) -> Option<usize> {
        let window_region = pointer_region.map(|region| PointerRegion {
            x: self.strip_origin.0 + region.x,
            y: self.strip_origin.1 + region.y,
            ..region
        });
        self.begin_close_in_window(target, window_region, reduced_motion, now)
    }

    /// Consecutive Chrome-style close: the hotspot is already in window
    /// coordinates and must not be rebased onto whichever band owns the strip
    /// after a pane resize or ownership handoff.
    pub(crate) fn begin_close_in_window(
        &mut self,
        target: &Target,
        pointer_region: Option<PointerRegion>,
        reduced_motion: bool,
        now: Instant,
    ) -> Option<usize> {
        let index = self.live.iter().position(|chip| &chip.target == target)?;
        let chip = self.live.remove(index);
        if reduced_motion {
            self.exits.clear();
            self.stable_close = None;
        } else {
            self.exits.push(ExitChip {
                target: chip.target,
                title: chip.title,
                x: self.strip_origin.0 + chip.x.value(now),
                y: self.strip_origin.1,
                width: chip.width,
                started: now,
            });
            self.stable_close = pointer_region.map(|region| StableClose {
                index,
                region,
                expires: now + transition_span(),
                pointer_inside: true,
            });
        }
        Some(index)
    }

    /// Permanently revoke a pointer forwarding token on its first hover leave.
    pub(crate) fn stable_region_hovered(&mut self, hovered: bool) {
        if !hovered {
            self.stable_close = None;
        }
    }

    /// Revoke only on real pointer movement outside the fixed window region.
    /// Layout remounts can emit hover-leave without any pointer motion; those
    /// must not destroy Chrome's same-coordinate consecutive-close target.
    pub(crate) fn stable_pointer_moved(&mut self, x: f32, y: f32) -> bool {
        let Some(guard) = self.stable_close else {
            return false;
        };
        let region = guard.region;
        if x < region.x
            || x > region.x + region.width
            || y < region.y
            || y > region.y + region.height
        {
            self.stable_region_hovered(false);
            return true;
        }
        false
    }

    /// The successor under the original pointer, while its one exit token is
    /// still live. The caller supplies current logical targets, so this cannot
    /// resurrect or address a tab that has already closed.
    pub(crate) fn stable_close_target(
        &mut self,
        targets: &[Target],
        now: Instant,
    ) -> Option<Target> {
        let guard = self.stable_close?;
        if !guard.pointer_inside || now > guard.expires {
            self.stable_close = None;
            return None;
        }
        targets.get(guard.index).cloned()
    }

    pub(crate) fn stable_close_region(&self, now: Instant) -> Option<PointerRegion> {
        self.stable_close
            .filter(|guard| guard.pointer_inside && now <= guard.expires)
            .map(|guard| guard.region)
    }

    pub(crate) fn transition_frame(
        &mut self,
        reduced_motion: bool,
        now: Instant,
    ) -> TabTransitionFrame {
        if reduced_motion {
            self.exits.clear();
            self.stable_close = None;
        } else {
            self.exits.retain(|exit| progress(exit.started, now) < 1.0);
            if self.stable_close.is_some_and(|guard| now > guard.expires) {
                self.stable_close = None;
            }
        }
        let exits = self
            .exits
            .iter()
            .map(|exit| {
                let t = progress(exit.started, now);
                ExitChipFrame {
                    target: exit.target.clone(),
                    title: exit.title.clone(),
                    x: exit.x,
                    y: exit.y,
                    width: exit.width * (1.0 - t),
                    opacity: 1.0 - t,
                }
            })
            .collect::<Vec<_>>();
        TabTransitionFrame {
            animating: !exits.is_empty(),
            exits,
            stable_close: self.stable_close_region(now),
        }
    }

    /// Reconcile chrome against the final logical strip and evaluate one frame.
    ///
    /// `controls_width` is the already-budgeted width of `+`, collapse and
    /// their gaps. Tab slots divide only the room left over, capped at the
    /// authored width; in a narrow strip they shrink all the way down and the
    /// title's normal truncation handles the result instead of overflowing the
    /// viewport.
    pub(crate) fn frame(
        &mut self,
        descriptors: &[TabDescriptor],
        available_width: f32,
        controls_width: f32,
        reduced_motion: bool,
        now: Instant,
    ) -> TabStripFrame {
        let gap = chip_gap(available_width, controls_width, descriptors.len());
        let width = chip_width(available_width, controls_width, gap, descriptors.len());
        let mut previous = std::mem::take(&mut self.live);
        let mut live = Vec::with_capacity(descriptors.len());
        for (index, descriptor) in descriptors.iter().enumerate() {
            let target_x = index as f32 * (width + gap);
            let key = descriptor.target.element_key();
            let found = previous
                .iter()
                .position(|chip| chip.target.element_key() == key)
                .map(|index| previous.remove(index));
            let mut chip = found.unwrap_or_else(|| LiveChip {
                target: descriptor.target.clone(),
                title: descriptor.title.clone(),
                x: ScalarMotion::resting(target_x, now),
                width,
            });
            chip.target = descriptor.target.clone();
            chip.title = descriptor.title.clone();
            chip.width = width;
            if reduced_motion {
                chip.x = ScalarMotion::resting(target_x, now);
            } else {
                chip.x.retarget(target_x, now);
            }
            live.push(chip);
        }
        self.live = live;

        let controls_target = descriptors.len() as f32 * (width + gap);
        let controls_x = self
            .controls_x
            .get_or_insert_with(|| ScalarMotion::resting(controls_target, now));
        if reduced_motion {
            *controls_x = ScalarMotion::resting(controls_target, now);
            self.exits.clear();
            self.stable_close = None;
        } else {
            controls_x.retarget(controls_target, now);
        }

        self.exits
            .retain(|exit| progress(exit.started, now) < 1.0 && !reduced_motion);
        if self
            .stable_close
            .is_some_and(|guard| now > guard.expires || reduced_motion)
        {
            self.stable_close = None;
        }

        let live_frames = self
            .live
            .iter()
            .enumerate()
            .map(|(index, chip)| {
                let target_x = index as f32 * (width + gap);
                LiveChipFrame {
                    target: chip.target.clone(),
                    title: chip.title.clone(),
                    x_offset: chip.x.value(now) - target_x,
                    width,
                }
            })
            .collect();
        let animating = self.live.iter().any(|chip| chip.x.moving(now)) || controls_x.moving(now);
        TabStripFrame {
            live: live_frames,
            gap,
            controls_x_offset: controls_x.value(now) - controls_target,
            controls_x: controls_x.value(now),
            animating,
        }
    }
}

fn chip_gap(available_width: f32, controls_width: f32, tabs: usize) -> f32 {
    if tabs == 0 {
        return 0.0;
    }
    ((available_width.max(0.0) - controls_width.max(0.0)).max(0.0) / tabs as f32).min(CHIP_GAP)
}

fn chip_width(available_width: f32, controls_width: f32, gap: f32, tabs: usize) -> f32 {
    if tabs == 0 {
        return 0.0;
    }
    let available = if available_width.is_finite() {
        available_width.max(0.0)
    } else {
        0.0
    };
    let controls = if controls_width.is_finite() {
        controls_width.max(0.0)
    } else {
        0.0
    };
    // One gap follows the last tab before the control group as well.
    let gaps = gap * tabs as f32;
    ((available - controls - gaps).max(0.0) / tabs as f32).min(CHIP_MAX_WIDTH)
}

fn transition_span() -> Duration {
    motion::span(&TAB_SLIDE)
}

fn progress(started: Instant, now: Instant) -> f32 {
    motion::exit_progress_at(&TAB_SLIDE, started, now)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewTabChoice {
    Universe,
    Stage,
    Pattern,
    Track,
}

impl NewTabChoice {
    pub(crate) const ALL: [Self; 4] = [Self::Universe, Self::Stage, Self::Pattern, Self::Track];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Universe => "Universe setup",
            Self::Stage => "Stage builder",
            Self::Pattern => "Pattern editor",
            Self::Track => "Track editor",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NewTabPrerequisites {
    pub(crate) venue: Option<String>,
    pub(crate) track: Option<String>,
    pub(crate) pattern: Option<String>,
    /// The track a graph tab would be evaluated against, resolved from the
    /// tab strip (`Luma::graph_track_context`). The graph editor cannot open
    /// without one — §6/§9 ruling 1 of the graph-editor design doc.
    pub(crate) graph_track: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChoiceAvailability {
    pub(crate) choice: NewTabChoice,
    pub(crate) reason: Option<&'static str>,
}

impl ChoiceAvailability {
    pub(crate) fn enabled(self) -> bool {
        self.reason.is_none()
    }
}

pub(crate) fn menu_choices(prerequisites: &NewTabPrerequisites) -> [ChoiceAvailability; 4] {
    NewTabChoice::ALL.map(|choice| {
        let reason = match choice {
            NewTabChoice::Universe | NewTabChoice::Stage if prerequisites.venue.is_none() => {
                Some("Select a venue first")
            }
            NewTabChoice::Track if prerequisites.venue.is_none() => Some("Select a venue first"),
            NewTabChoice::Track if prerequisites.track.is_none() => Some("Select a track first"),
            // The track gate outranks the pattern gate: a pattern can be
            // picked while trackless, but no pick makes the editor openable
            // without a track to evaluate against.
            NewTabChoice::Pattern if prerequisites.graph_track.is_none() => {
                Some(crate::graph::NO_TRACK_REASON)
            }
            NewTabChoice::Pattern if prerequisites.pattern.is_none() => {
                Some("Select a pattern first")
            }
            _ => None,
        };
        ChoiceAvailability { choice, reason }
    })
}

impl Luma {
    /// What the current shell state lets the `+` menu (and the empty panel's
    /// copy of it) offer. One constructor, so the two drawings of the choices
    /// cannot gate on different facts.
    pub(crate) fn new_tab_prerequisites(&self) -> NewTabPrerequisites {
        NewTabPrerequisites {
            venue: self
                .sidebar
                .as_ref()
                .map(|state| state.venue_id().to_string()),
            track: self.selected_track.clone(),
            pattern: self
                .selected_pattern
                .as_ref()
                .map(|pattern| pattern.id.clone()),
            graph_track: self.graph_track_context().map(|context| context.track),
        }
    }

    /// One close path for the chip, middle click and keyboard. Logical removal
    /// and teardown happen immediately; this module retains only exit paint.
    pub(crate) fn close_tab(
        &mut self,
        target: &Target,
        pointer_region: Option<PointerRegion>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.tab_chrome.begin_close(
            target,
            pointer_region,
            motion::reduced_motion(cx),
            Instant::now(),
        );
        self.finish_close_tab(target, cx);
    }

    pub(crate) fn close_tab_at_window_region(
        &mut self,
        target: &Target,
        pointer_region: PointerRegion,
        cx: &mut gpui::Context<Self>,
    ) {
        self.tab_chrome.begin_close_in_window(
            target,
            Some(pointer_region),
            motion::reduced_motion(cx),
            Instant::now(),
        );
        self.finish_close_tab(target, cx);
    }

    /// Logical teardown, shared by every close gesture.
    ///
    /// **The panel's width is not part of a close.** Closing the last tab used
    /// to snap it to zero — a leftover from when emptiness hid the panel — and
    /// the next frame's `retarget` read that as "open from nothing", so the
    /// empty state arrived on a full slide-in that nobody had asked for. What
    /// changed is which tabs are open; the region the user sized stays where
    /// they put it.
    fn finish_close_tab(&mut self, target: &Target, cx: &mut gpui::Context<Self>) {
        if let Some(body) = self.workspace.close(target) {
            self.teardown(body, cx);
        }
        cx.notify();
    }

    /// Act on the currently named subject. Each opener is target-idempotent;
    /// this layer only translates one menu choice into that existing path.
    pub(crate) fn activate_new_tab_choice(
        &mut self,
        choice: NewTabChoice,
        cx: &mut gpui::Context<Self>,
    ) {
        self.tab_chrome.menu_open = false;
        match choice {
            NewTabChoice::Universe => self.open_universe(cx),
            NewTabChoice::Stage => self.open_stage(cx),
            NewTabChoice::Pattern => {
                if let Some(pattern) = self.selected_pattern.clone() {
                    self.open_pattern(pattern, cx);
                }
            }
            NewTabChoice::Track => {
                if let Some(track) = self.selected_track.clone() {
                    self.open_track(&track, cx);
                }
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str) -> Target {
        Target::Graph {
            pattern: name.into(),
        }
    }

    fn tabs(names: &[&str]) -> Vec<TabDescriptor> {
        names
            .iter()
            .map(|name| TabDescriptor {
                target: target(name),
                title: (*name).into(),
            })
            .collect()
    }

    #[test]
    fn close_contracts_exit_and_glides_neighbor_into_the_released_slot() {
        let start = Instant::now();
        let mut chrome = TabChrome::default();
        chrome.frame(&tabs(&["a", "b", "c"]), 600.0, 60.0, false, start);
        assert_eq!(
            chrome.begin_close(&target("b"), None, false, start),
            Some(1)
        );

        let beginning = chrome.frame(&tabs(&["a", "c"]), 600.0, 60.0, false, start);
        let beginning_exit = chrome.transition_frame(false, start);
        assert_eq!(beginning_exit.exits.len(), 1);
        assert!(beginning.live[1].x_offset > 100.0);
        assert_eq!(beginning_exit.exits[0].opacity, 1.0);

        let middle = chrome.frame(
            &tabs(&["a", "c"]),
            600.0,
            60.0,
            false,
            start + transition_span() / 2,
        );
        let middle_exit = chrome.transition_frame(false, start + transition_span() / 2);
        assert!(middle.live[1].x_offset > 0.0);
        assert!(middle.live[1].x_offset < beginning.live[1].x_offset);
        assert!(middle_exit.exits[0].width < beginning_exit.exits[0].width);
        assert!(middle_exit.exits[0].opacity < beginning_exit.exits[0].opacity);

        let end = chrome.frame(
            &tabs(&["a", "c"]),
            600.0,
            60.0,
            false,
            start + transition_span(),
        );
        assert!(chrome
            .transition_frame(false, start + transition_span())
            .exits
            .is_empty());
        assert!(end.live[1].x_offset.abs() <= 0.5);
    }

    #[test]
    fn stable_pointer_region_forwards_once_but_never_rearms_after_leave() {
        let start = Instant::now();
        let region = PointerRegion {
            x: 8.0,
            y: 5.0,
            width: 14.0,
            height: 14.0,
        };
        let mut chrome = TabChrome::default();
        chrome.frame(&tabs(&["a", "b", "c"]), 600.0, 60.0, false, start);
        chrome.begin_close(&target("b"), Some(region), false, start);
        assert_eq!(
            chrome.stable_close_target(&[target("a"), target("c")], start),
            Some(target("c"))
        );
        assert_eq!(chrome.stable_close_region(start), Some(region));

        assert!(!chrome.stable_pointer_moved(region.x + 1.0, region.y + 1.0));
        assert_eq!(chrome.stable_close_region(start), Some(region));
        assert!(chrome.stable_pointer_moved(region.x + region.width + 1.0, region.y + 1.0));
        assert!(!chrome.stable_pointer_moved(region.x + region.width + 2.0, region.y + 1.0));
        assert_eq!(
            chrome.stable_close_target(&[target("a"), target("c")], start),
            None
        );
    }

    #[test]
    fn pointer_move_without_a_stable_guard_is_a_noop() {
        let mut chrome = TabChrome::default();
        assert!(!chrome.stable_pointer_moved(10.0, 10.0));
    }

    #[test]
    fn reduced_motion_snaps_to_final_strip_without_exit_state() {
        let start = Instant::now();
        let mut chrome = TabChrome::default();
        chrome.frame(&tabs(&["a", "b", "c"]), 600.0, 60.0, false, start);
        chrome.begin_close(&target("b"), None, true, start);
        let frame = chrome.frame(&tabs(&["a", "c"]), 600.0, 60.0, true, start);
        assert!(chrome.transition_frame(true, start).exits.is_empty());
        assert!(frame.live.iter().all(|chip| chip.x_offset == 0.0));
        assert!(!frame.animating);
    }

    #[test]
    fn narrow_strip_shrinks_slots_instead_of_exceeding_available_width() {
        let start = Instant::now();
        let mut chrome = TabChrome::default();
        let frame = chrome.frame(&tabs(&["a", "b", "c"]), 120.0, 60.0, true, start);
        let occupied = frame.live.iter().map(|chip| chip.width).sum::<f32>()
            + frame.gap * frame.live.len() as f32
            + 60.0;
        assert!(occupied <= 120.0 + f32::EPSILON);
        assert!(frame.live[0].width < CHIP_MAX_WIDTH);
    }

    #[test]
    fn extreme_strip_contracts_gaps_before_clipping_the_reserved_add_control() {
        let start = Instant::now();
        let mut chrome = TabChrome::default();
        let frame = chrome.frame(&tabs(&["a", "b", "c"]), 35.0, 24.0, true, start);
        let tabs_and_gaps = frame.live.iter().map(|chip| chip.width).sum::<f32>()
            + frame.gap * frame.live.len() as f32;
        assert!(tabs_and_gaps + 24.0 <= 35.0 + f32::EPSILON);
        assert!(frame.gap < CHIP_GAP);
    }

    #[test]
    fn close_transition_stays_in_window_space_when_strip_owner_moves() {
        let start = Instant::now();
        let mut chrome = TabChrome::default();
        chrome.set_strip_origin(100.0, 7.0);
        chrome.frame(&tabs(&["a", "b", "c"]), 600.0, 60.0, false, start);
        let local = PointerRegion {
            x: 160.0,
            y: 5.0,
            width: 14.0,
            height: 14.0,
        };
        chrome.begin_close(&target("b"), Some(local), false, start);
        let before = chrome.transition_frame(false, start);
        let exit_x = before.exits[0].x;
        assert_eq!(before.exits[0].y, 7.0);
        assert_eq!(before.stable_close.unwrap().x, 260.0);
        assert_eq!(before.stable_close.unwrap().y, 12.0);

        chrome.set_strip_origin(500.0, 7.0);
        let after_handoff = chrome.transition_frame(false, start);
        assert_eq!(after_handoff.exits[0].x, exit_x);
        assert_eq!(after_handoff.stable_close, before.stable_close);
    }

    #[test]
    fn menu_reasons_name_each_missing_prerequisite() {
        let none = menu_choices(&NewTabPrerequisites::default());
        assert_eq!(none[0].reason, Some("Select a venue first"));
        assert_eq!(none[1].reason, Some("Open a track to edit patterns"));
        assert_eq!(none[2].reason, Some("Select a venue first"));

        let venue = NewTabPrerequisites {
            venue: Some("v".into()),
            ..Default::default()
        };
        let venue_only = menu_choices(&venue);
        assert!(venue_only[0].enabled());
        assert_eq!(venue_only[2].reason, Some("Select a track first"));

        // The track gate outranks the pattern gate; with a track editor open
        // the pattern gate is what remains.
        let track_open = NewTabPrerequisites {
            venue: Some("v".into()),
            graph_track: Some("t".into()),
            ..Default::default()
        };
        assert_eq!(
            menu_choices(&track_open)[1].reason,
            Some("Select a pattern first")
        );

        let all = menu_choices(&NewTabPrerequisites {
            venue: Some("v".into()),
            track: Some("t".into()),
            pattern: Some("p".into()),
            graph_track: Some("t".into()),
        });
        assert!(all.into_iter().all(ChoiceAvailability::enabled));
    }
}
