//! Keeping the rig's installed scene in step with the working copy.

use super::*;

/// Keep the render engine's installed scene in step with the working copy.
///
/// The visualizer samples an *installed* scene ([`Library::sample_universe`]),
/// and installing is a command — so without this, every edit the timeline made
/// was invisible on the rig until the view was re-opened. Reconciled rather
/// than issued per gesture: the render pass is the one place every path that
/// can change a clip has already converged, so no edit site has to remember to
/// invalidate, and a frame's worth of picker motion is one install.
///
/// The comparison is the *scene's* inputs, not only the list's identity: a
/// write landing re-resolves the working copy into an equal list, and
/// re-compiling for that would put one composite behind every save — see
/// [`stale`].
///
/// Which *document* is installed is not this reconcile's question — a score
/// switch is the stage's ([`crate::visualizer::Visualizer::relight`]), which
/// is also the only path that survives the editor tab not being the one on
/// screen. This keeps the open score's scene abreast of edits to it.
pub(super) fn sync_composite(editor: &mut Editor, cx: &mut Context<Luma>) {
    if editor.compositing {
        return;
    }
    let Some(last) = editor.composited.as_mut() else {
        return;
    };
    if !stale(last, &editor.clips) {
        return;
    }
    let score = editor.score.id.clone();
    let candidate = match editor.graph_candidate() {
        Ok(score) => score,
        Err(error) => {
            editor.error = Some(error);
            return;
        }
    };
    let sent = editor.clips.clone();
    let target = editor.target();
    editor.compositing = true;
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| {
            this.library.composite_score_document(&score, &candidate)
        }) else {
            return;
        };
        let result = pending.await;
        this.update(cx, |this, cx| {
            this.edit_track_tab(&target, cx, |editor| {
                editor.compositing = false;
                // Remember this attempt, including a failure, so a bad input
                // produces one useful error instead of a retry every frame.
                editor.composited = Some(sent);
                if let Err(error) = result {
                    editor.error = Some(format!("Playback: {error}"));
                }
            });
        })
        .ok();
    })
    .detach();
}

/// Does the working copy `clips` light differently from the `installed` one?
///
/// Asked on every frame, so the answer is usually one pointer compare:
/// [`Editor::replace_clips`] always swaps in a fresh list, so the same list is
/// an unedited one. A new list that compiles to the same scene — a write
/// landing re-resolves the working copy into an equal one — is adopted as
/// installed, so it is compared field by field once rather than every frame.
fn stale(installed: &mut Rc<[Clip]>, clips: &Rc<[Clip]>) -> bool {
    if Rc::ptr_eq(installed, clips) {
        return false;
    }
    if same_scene(installed, clips) {
        *installed = Rc::clone(clips);
        return false;
    }
    true
}

/// Do these two lists compile to the same scene?
///
/// Every field the compositor reads and no others — a clip's lane, label and
/// colour are pictures of it, and moving one lights nothing differently.
fn same_scene(a: &[Clip], b: &[Clip]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.id == b.id
                && a.start == b.start
                && a.end == b.end
                && a.z == b.z
                && a.blend == b.blend
                && a.core == b.core
        })
}

#[cfg(test)]
mod tests {
    use super::{stale, BlendMode, Clip};
    use std::rc::Rc;

    fn clip(id: &str, start: f64) -> Clip {
        Clip {
            id: id.to_owned().into(),
            output: "color".into(),
            label: id.to_owned().into(),
            summary: Default::default(),
            color: luma_ui::ladder::pattern("color"),
            start,
            end: start + 1.,
            row: 0,
            z: 0,
            blend: BlendMode::Replace,
            core: None,
        }
    }

    #[test]
    fn an_edit_reaches_the_rig_and_an_equal_list_does_not() {
        let mut installed: Rc<[Clip]> = vec![clip("a", 0.), clip("b", 2.)].into();
        let unchanged = Rc::clone(&installed);
        assert!(!stale(&mut installed, &unchanged));

        // A re-read that resolves to the same scene is not a recompile, and
        // is installed in its place.
        let reread: Rc<[Clip]> = installed.to_vec().into();
        assert!(!stale(&mut installed, &reread));
        assert!(Rc::ptr_eq(&installed, &reread));

        // An edit after that adoption is still seen.
        let mut moved = reread.to_vec();
        moved[1].start = 3.;
        let edited: Rc<[Clip]> = moved.into();
        assert!(stale(&mut installed, &edited));
        assert!(
            stale(&mut installed, &edited),
            "nothing installs it but a composite"
        );

        // A lane or label is a picture of a clip, not an input to the scene.
        let mut relabelled = reread.to_vec();
        relabelled[0].label = "renamed".into();
        relabelled[0].row = 4;
        assert!(!stale(&mut installed, &relabelled.into()));
    }
}
