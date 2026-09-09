//! The timeline's animation boundary. Playback never notifies the application root.
use super::*;

pub(super) struct Surface {
    app: WeakEntity<Luma>,
    scheduled: bool,
}
impl Surface {
    pub fn new(app: WeakEntity<Luma>) -> Self {
        Self {
            app,
            scheduled: false,
        }
    }
}
impl Render for Surface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div();
        };
        let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
            return div();
        };
        let surface = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(minimap::element(editor, &app))
            .child(canvas_element(editor, &app));
        if (editor.transport.playing || editor.zoom_motion.is_some()) && !self.scheduled {
            self.scheduled = true;
            let this = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let Some(this) = this.upgrade() else {
                    return;
                };
                this.update(cx, |surface, cx| {
                    surface.scheduled = false;
                    let Some(app) = surface.app.upgrade() else {
                        return;
                    };
                    let surface_id = cx.entity_id();
                    let changed = app.update(cx, |app, _| {
                        let Some(Body::TrackEditor(editor)) = app.workspace.active_body_mut()
                        else {
                            return false;
                        };
                        // A parked tab's pending callback cannot animate a different editor.
                        if editor.playback_surface.as_ref().map(Entity::entity_id)
                            != Some(surface_id)
                        {
                            return false;
                        }
                        let zoomed = editor.tick_zoom(std::time::Instant::now());
                        if !editor.transport.playing {
                            return zoomed;
                        }
                        if !matches!(editor.gesture, Some(Gesture::Scrub)) {
                            if let Some(position) =
                                editor.transport.clock.position(std::time::Instant::now())
                            {
                                editor.transport.position =
                                    position.min(f64::from(editor.transport.duration)) as f32;
                                editor.follow_playhead();
                            }
                        }
                        true
                    });
                    if changed {
                        cx.notify();
                    }
                });
            });
        }
        surface
    }
}
