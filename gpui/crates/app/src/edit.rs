//! The editing verbs, sent to whatever has the keyboard.
//!
//! Undo, Cut and the rest are one action each, so the Edit menu has one item
//! per verb and shows the key that does it. What the verb acts on is the
//! focus path's to say, the way a binding's scope is: a text field takes its
//! own, the timeline and the clip graph take the score's, the stage builder
//! takes the room's. A path with none of them does nothing.

use gpui::{Action, Context, Window};
use luma_ui::text_input;

use crate::keymap::context;
use crate::Luma;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verb {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

impl Verb {
    /// The field's own action for the verb.
    fn in_field(self) -> Box<dyn Action> {
        match self {
            Verb::Undo => Box::new(text_input::Undo),
            Verb::Redo => Box::new(text_input::Redo),
            Verb::Cut => Box::new(text_input::Cut),
            Verb::Copy => Box::new(text_input::Copy),
            Verb::Paste => Box::new(text_input::Paste),
            Verb::SelectAll => Box::new(text_input::SelectAll),
        }
    }
}

impl Luma {
    pub(crate) fn edit(&mut self, verb: Verb, window: &mut Window, cx: &mut Context<Self>) {
        let path = window.context_stack();
        let has = |name: &str| path.iter().any(|entry| entry.contains(name));
        if has(context::TEXT_INPUT) {
            // The field is on the path but did not hear the shell's verb;
            // hand it its own.
            window.dispatch_action(verb.in_field(), cx);
            return;
        }
        let stage = has(context::STAGE);
        let clips = has(context::TRACK_EDITOR) || has(context::CLIP_GRAPH);
        let timeline = has(context::TRACK_EDITOR);
        match verb {
            Verb::Undo if stage => self.stage_undo(cx),
            Verb::Redo if stage => self.stage_redo(cx),
            Verb::Undo if clips => self.undo_clips(cx),
            Verb::Redo if clips => self.redo_clips(cx),
            Verb::Cut if timeline => self.cut_clips(cx),
            Verb::Copy if timeline => self.copy_clips(cx),
            Verb::Paste if timeline => self.paste_clips(cx),
            _ => {}
        }
    }
}
