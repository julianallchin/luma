//! The graph editor, open on a pattern of the Perform set, or on the preset
//! browser to start one.
//!
//! It is the track editor's sheet ([`sheet`]) hosted by a Perform tab: its
//! subject is the one pattern held here, a copy that runs ahead of the set
//! while its edits wait on their debounced save. The page takes the whole
//! tab until its way back closes it, saving what is owed.

use gpui::prelude::*;
use gpui::{div, AnyElement, Context, Entity, Window};
use luma_lib::models::perform::PerformPattern;

use super::Perform;
use crate::tabs::Target;
use crate::track_editor::sheet::{self, Host, HostMut};
use crate::Luma;

/// The graph editor of a Perform tab.
pub(crate) struct PatternSheet {
    /// The Perform tab it is open in.
    pub(crate) target: Target,
    /// The pattern being edited, ahead of the set by whatever save is owed.
    /// `None` while the preset browser picks one to start.
    pub(crate) pattern: Option<PerformPattern>,
    pub(crate) sheet: sheet::State,
}

impl PatternSheet {
    /// The pattern open in the editor; empty while it browses presets.
    pub(crate) fn pattern_id(&self) -> &str {
        self.pattern.as_ref().map_or("", |pattern| &pattern.id)
    }
}

impl Luma {
    /// Open the graph editor on `pattern_id` of Perform tab `target`.
    pub(crate) fn edit_perform_pattern(
        &mut self,
        target: &Target,
        pattern_id: &str,
        cx: &mut Context<Self>,
    ) {
        let pattern = self
            .perform_mut(target)
            .and_then(|state| state.pattern(pattern_id).cloned());
        if let Some(pattern) = pattern {
            self.open_pattern_sheet(target, Some(pattern), cx);
        }
    }

    /// Show the preset browser, with its previews, to start a pattern in
    /// the place whose "+" menu is open; picking one calls
    /// [`Luma::add_perform_pattern`].
    pub(crate) fn browse_perform_presets(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.open_pattern_sheet(target, None, cx);
    }

    /// Open the editor on `pattern`, or on the browser, in place of whatever
    /// it had open.
    fn open_pattern_sheet(
        &mut self,
        target: &Target,
        pattern: Option<PerformPattern>,
        cx: &mut Context<Self>,
    ) {
        self.close_pattern_sheet(target, cx);
        if let Some(state) = self.perform_mut(target) {
            state.pattern_sheet = Some(PatternSheet {
                target: target.clone(),
                pattern,
                sheet: sheet::State::default(),
            });
            cx.notify();
        }
    }

    /// Back to the surface from Perform tab `target`'s editor, saving what
    /// its edits still owe. Reports whether one was open, for the Escape
    /// ladder.
    pub(crate) fn close_pattern_sheet(&mut self, target: &Target, cx: &mut Context<Self>) -> bool {
        let Some(open) = self
            .perform_mut(target)
            .and_then(|state| state.pattern_sheet.as_ref())
        else {
            return false;
        };
        let owed = open.sheet.owes_write();
        if owed {
            self.flush_sheet(target, cx);
        }
        if let Some(state) = self.perform_mut(target) {
            state.pattern_sheet = None;
        }
        cx.notify();
        true
    }
}

/// Bring the open editor's controls up to date with its pattern, before the
/// tab draws.
pub(crate) fn sync(state: &mut Perform, window: &mut Window, cx: &mut Context<Luma>) {
    if let Some(open) = state.pattern_sheet.as_mut() {
        sheet::sync(HostMut::Pattern(open), window, cx);
    }
}

/// The editor page, in place of the grid while a pattern is open.
pub(crate) fn page(state: &Perform, target: &Target, app: &Entity<Luma>) -> Option<AnyElement> {
    let open = state.pattern_sheet.as_ref()?;
    let target = target.clone();
    // Back to the view it was opened from.
    let (label, agent) = match state.surface.view {
        super::surface::View::Perform => ("Perform", "Back to Perform"),
        super::surface::View::Setup => ("Setup", "Back to Setup"),
    };
    let back = sheet::back("pattern-sheet-back", label, agent, app, move |this, cx| {
        this.close_pattern_sheet(&target, cx);
    });
    Some(
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(sheet::panel(Host::Pattern(open), app, Some(back)))
            .into_any_element(),
    )
}
