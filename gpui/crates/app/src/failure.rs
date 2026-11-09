//! When something the user just did did not land: one quiet line in the
//! status bar, in plain words.
//!
//! These are rare — most are bugs — so no screen keeps an error of its own or
//! draws one inside a panel. The caller says what did not happen ("Haze was
//! not saved"); the cause, which is usually a command name and a backend
//! error, goes to the log and nowhere else. The line clears itself after a
//! while, or when it is pressed, and a newer one replaces it.

use std::fmt::Display;
use std::time::Duration;

use gpui::{Context, SharedString};

use crate::Luma;

/// How long a line stays up unless pressed. Long enough to read after looking
/// down from whatever the hand was doing.
const SHOWN_FOR: Duration = Duration::from_secs(8);

#[derive(Default)]
pub(crate) struct Failure {
    text: Option<SharedString>,
    /// Which report is up, so an older report's timer cannot clear a newer one.
    generation: u64,
}

impl Luma {
    /// Say in the status bar that `what` did not happen, and log why.
    pub(crate) fn report_failure(
        &mut self,
        what: impl Into<SharedString>,
        cause: impl Display,
        cx: &mut Context<Self>,
    ) {
        let what = what.into();
        log::warn!("{what}: {cause}");
        self.report(what, cx);
    }

    /// Say in the status bar that something the user tried cannot be done,
    /// when the words are the whole of it and there is no cause to log.
    pub(crate) fn report(&mut self, what: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.failure.generation += 1;
        self.failure.text = Some(what.into());
        let generation = self.failure.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SHOWN_FOR).await;
            this.update(cx, |this, cx| {
                if this.failure.generation == generation {
                    this.failure.text = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

/// [`Luma::report_failure`] from code that holds part of the app borrowed —
/// an editor taken out of a body — rather than the app itself: the report
/// lands once that borrow is over.
pub(crate) fn report_later(
    cx: &mut Context<Luma>,
    what: &'static str,
    cause: impl Display + 'static,
) {
    let app = cx.entity().downgrade();
    cx.defer(move |cx| {
        app.update(cx, |this, cx| this.report_failure(what, cause, cx))
            .ok();
    });
}

/// The status bar's line, while one is up. Pressing it puts it away.
pub(crate) fn item(app: &Luma) -> Option<crate::status_bar::Item> {
    let text = app.failure.text.clone()?;
    let mut item = crate::status_bar::Item::new("failure", text);
    item.attention = true;
    item.press = Some(|this, cx| {
        this.failure.text = None;
        cx.notify();
    });
    Some(item)
}
