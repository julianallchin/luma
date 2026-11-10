# Visualizer fullscreen

Status: built (`gpui/crates/app/src/fullscreen.rs`). Fullscreen uses the current window; there is no detached visualizer window.

The visualizer fills the current display for watching a score or examining the rig. Entering and leaving feel like expanding the view already on screen, with playback, camera position, selection, and the editing layout preserved. The sections below are the behaviour it is held to.

## Entry and exit

- A compact Nucleo expand icon sits in the visualizer’s upper-right corner. Tooltip and accessible name: **Fullscreen visualizer**. It uses the shared rounded button primitive and subtle hover fill.
- **View → Fullscreen visualizer** does the same. Shortcut: **Shift+F**, scoped to the visualizer and excluded while typing. **F** keeps its frame-selection behavior.
- Fullscreen fills the display containing the current window using native fullscreen. The stage covers the whole window: the title bar, agent panel, status bar, the mode's body (the timeline, the venue page or the Live grid), inspectors and split handles are hidden for the duration.
- In fullscreen, a Nucleo contract icon replaces the expand icon in the same corner, with tooltip and accessible name **Exit fullscreen**. It stays visibly available; the way out does not require hover to discover.
- **Escape** closes an open settings popover first, then exits fullscreen. Exiting must not clear the selected clip or alter a stage selection. Native OS fullscreen exit must restore the editor too.
- Exit returns to the exact prior window bounds and panel layout. If the app was already in native fullscreen, it returns to the editor within fullscreen instead of forcing it into a window.

## Fullscreen controls

The scene is the main surface. Keep orbit, pan, zoom, frame selection, and the existing environment/settings dock. Fullscreen is a viewing mode: venue-authoring tools, transform gizmos, and inspectors stay hidden, and shortcuts for hidden editors cannot modify their documents.

When the active view is playing or can play a track, show a small bottom-centred transport with **Play/Pause**, elapsed time, and duration. Space toggles that track when no text field is focused. Preserve the current playing/paused state on entry and exit; never start, stop, or restart playback merely because the view changed size.

For a venue or standalone pattern view without track transport, omit the centre bar entirely. Leave the existing environment dock at the lower right. Keep the exit button and settings dock in the same positions in both presentation states. Controls stay visible.

## Motion and continuity

- Use the shared surface motion (`luma_ui::motion::SURFACE`) for the expansion. Coordinate it with the OS fullscreen transition; do not play two successive expansion animations.
- Keep the live visualizer and playback session intact throughout the transition. No black reload frame, restarted effects, or duplicate render loop.
- Preserve the camera’s position, target, and zoom. Update the projection for the new aspect ratio; do not automatically frame the rig again.
- Keep selected clips and fixture selections in memory while their UI is hidden. Restore focus to the fullscreen trigger or its originating visualizer on exit.
- Respect reduced motion. Escape and the exit control remain available during transitions and scene loading/error states.
- Treat fullscreen as temporary window presentation state, not a venue/score setting. A fresh launch opens the normal workspace.

## Empty bottom bar

The toolbar (`overlay_toolbar` in `gpui/crates/app/src/visualizer/chrome.rs`) mounts its bar only when at least one control is present. With no controls there must be no background, blur, border, padding, hitbox, or accessibility node. This applies to the embedded view as well as fullscreen. A populated venue-editing toolbar must continue to appear in the embedded view.

The `Visualizer::view_finder` inset follows the centre bar’s actual presence, so an absent bar also releases its framing space. This affects explicit camera fitting; it does not automatically move an existing camera when controls change.

## Review criteria

1. Enter fullscreen from a playing score; audio, playback time, lighting, and camera continue without a restart.
2. Exit with the button, Escape, and the OS fullscreen gesture; each restores the prior layout and window state.
3. Enter with a clip selected; the inspector disappears for fullscreen and returns with that selection on exit.
4. A track view has usable transport; a view without transport has no centre-bar decoration or invisible click blocker.
5. Camera and settings interactions work. Hidden timeline, graph, and venue-authoring shortcuts are inactive.
6. Popovers consume Escape before fullscreen; fullscreen consumes it before selection clearing. Keyboard focus remains usable on entry and exit.
7. Test reduced motion, rapid entry/exit, window-already-fullscreen, scene errors, and changing display size.

## Not built: detached window

A separate visualizer window would solve a different workflow: editing on one display while watching the rig on another. Fullscreen does not do that. An **Open visualizer in new window** action would need explicit ownership of playback, camera, and window lifetime.
