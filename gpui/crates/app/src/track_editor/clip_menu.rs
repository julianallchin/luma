//! A clip's menu: what a right-click on a clip offers, and the reader's
//! verdict on the clip that it records.

use luma_lib::database::local::feedback::{Feedback, Target as FeedbackTarget, Verdict};

use super::*;

/// The open menu: the clip it is about, and where the press landed.
pub(super) struct ClipMenu {
    clip: SharedString,
    /// Window space, which is what [`luma_ui::menu::ContextMenu`] hangs from
    /// and what an insertion is worked out from.
    at: Point<Pixels>,
}

/// The clips a score's stored verdicts call bad.
pub(super) fn bad_clips(verdicts: Vec<Feedback>) -> HashSet<SharedString> {
    verdicts
        .into_iter()
        .filter(|feedback| feedback.verdict == Verdict::Bad)
        .filter_map(|feedback| match feedback.target {
            FeedbackTarget::Clip { clip_id } => Some(clip_id.into()),
            FeedbackTarget::Turn { .. } => None,
        })
        .collect()
}

impl Luma {
    /// A right-click: open the menu of the clip under it. Reports whether
    /// there was one, so a press on empty lane can offer an insertion.
    pub(super) fn open_clip_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body_mut() else {
            return false;
        };
        let Some(clip) = editor.clip_under(at).map(|clip| clip.id.clone()) else {
            return false;
        };
        editor.clip_menu = Some(ClipMenu { clip, at });
        cx.notify();
        true
    }

    /// Close an open clip menu, if there is one: its rung of the Escape
    /// ladder, and what a press outside it does.
    pub(crate) fn dismiss_clip_menu(&mut self) -> bool {
        match self.workspace.active_body_mut() {
            Some(Body::TrackEditor(state)) => state.clip_menu.take().is_some(),
            _ => false,
        }
    }

    /// Record that the reader calls `clip` bad, or take that back. The clip
    /// itself is untouched: deleting it is its own command.
    fn mark_clip(&mut self, clip: SharedString, bad: bool, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body_mut() else {
            return;
        };
        let target = editor.target();
        let mark = move |editor: &mut Editor, clip: SharedString, bad: bool| {
            if bad {
                editor.bad_clips.insert(clip);
            } else {
                editor.bad_clips.remove(&clip);
            }
        };
        mark(editor, clip.clone(), bad);
        let stored = self.library.set_feedback(
            FeedbackTarget::Clip {
                clip_id: clip.to_string(),
            },
            bad.then_some(Verdict::Bad),
        );
        cx.spawn(async move |this, cx| {
            let Err(error) = stored.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.edit_track_tab(&target, cx, |editor| mark(editor, clip, !bad));
                this.report_failure("Could not save your feedback", error, cx);
            })
            .ok();
        })
        .detach();
    }
}

/// The open clip menu: an insertion where the press landed, as a right-click
/// on empty lane offers directly, and the reader's verdict on the clip.
pub(super) fn clip_menu(state: &Editor, app: &Entity<Luma>) -> Option<AnyElement> {
    let ClipMenu { clip, at } = state.clip_menu.as_ref()?;
    let mut menu = luma_ui::menu::ContextMenu::new("clip-menu", *at);
    if state.score_editable() {
        let (app, at) = (app.clone(), *at);
        menu = menu
            .item("Insert pattern", move |_, cx| {
                app.update(cx, |this, cx| this.timeline_insert_menu(at, cx));
            })
            .separator();
    }
    let bad = state.bad_clips.contains(clip);
    let (marked, clip) = (app.clone(), clip.clone());
    menu = menu.item(
        if bad { "Unmark as bad" } else { "Mark as bad" },
        move |_, cx| {
            marked.update(cx, |this, cx| this.mark_clip(clip.clone(), !bad, cx));
        },
    );
    let app = app.clone();
    Some(menu.render(move |_, cx| {
        app.update(cx, |this, cx| {
            if this.dismiss_clip_menu() {
                cx.notify();
            }
        });
    }))
}
