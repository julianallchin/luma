# Venue workspace — native revision

The venue lives in the right workspace column, next to chat. It starts at 65% of the shared width, with a 360 px minimum for chat and a draggable divider. A live stage preview sits above compact editing controls. The tone is professional: short labels, direct manipulation and contextual controls, without tutorial copy. It must remain useful at the shell's 320 px workspace minimum.

## Implemented

- One venue tab owns Stage, Fixtures and Groups. It opens on Stage. Stage no longer creates a separate tab or takes the whole workspace height.
- Fixtures shows a compact selectable list. Add fixtures uses the existing fixture picker and allocator. Selecting lights highlights them in the stage; selecting fixtures in the stage updates the list.
- Stage keeps the existing placement, snapping, distribution, duplicate, flip and undo tools. Environment and selection controls sit below the preview. A flat element list selects the same objects as the viewport; the inspector exposes Duplicate and Remove. Removal uses the native confirmation dialog. Removing structure preserves attached fixtures as unplaced inventory, and undo restores the structure; deleting a fixture directly still removes its patch row. Native palettes can use more window width when opened.
- Groups supports creation, staged membership edits, renaming and changing the parent group. The same fixture can be in multiple authored groups. Automatic groups are visible alongside authored groups, marked Auto. Parent paths distinguish same-named leaves without requiring an expanded tree. Clicking a group highlights its fixtures in the stage. Parent changes organize the group list; moving physical structures remains a stage operation.
- A group save is one backend transaction. A missing fixture, name conflict or permission failure rolls back the whole edit. Cancelling discards the draft.
- Patch details retains addresses, modes, footprint, auto patch and local output routing. The technical table remains horizontally scrollable; it is no longer the first thing a venue opens into.
- No schema migration or rendering changes. Ambient occlusion remains future renderer work.

## Important limits / next pass

Group selector names are currently the strings saved effects reference. The native save command and existing rename commands refuse to change a name mentioned in saved work. Membership remains editable. The reference check is deliberately conservative and also protects names found in graph labels or legacy implementations. The complete solution is stable group selector identity or an authored-history transaction updating every affected score, rather than silently making an existing effect select nothing.

The first membership editor operates on whole fixtures. Existing per-head memberships are retained unless their fixture is explicitly removed, but individual heads still need their own editor and partial-membership indicator. Derived/merged groups need clearer membership provenance before exposing arbitrary membership changes.

Advanced patch editing is still the old wide table behind Patch details. A focused fixture detail sheet is the next step, along with moving the existing Renderer Lab control out of the default preview header.

The original concept image in this directory was generated. The native captures are actual GPUI windows using the user's EBF library on a dedicated X11 display; they are not web mockups. No in-app agent/model turns were used.

## Validation

- Native group workflow at 1100 × 900: create, assign, edit, rename, reparent, cancel, and switch sections.
- Existing patch tests cover address refusal, occupancy, outputs, auto patch, destructive confirmation, adding fixtures and mode changes.
- Existing builder tests cover placement, attachments, distribution, fit refusal, duplicate/flip, detach, palette focus and undo/redo. The duplicate test now uses the platform's secondary modifier instead of hard-coding macOS Command.
- Backend tests cover atomic rollback, rename with membership changes, generated-group reparenting, cycle refusal and protecting names used by saved scores. Native tests cover element selection, confirmed removal, retained fixture inventory and undo, plus the wider workspace split and its resizing limits.
- Workspace/all-targets check and native application build.

The widened-shell checks pass, as do all 15 builder tests, 8 patch/group UI tests and 19 backend group tests. The all-targets check and native build pass. An additional venue-picker stale-response test fails while reopening its catalogue (`venue_launch_picker_create_and_stale_reads_are_correlated`); its injected catalogue failure arrives earlier than the test expects. That separate picker/request-order issue was not changed in this revision. The track-list ordering test passed on an isolated rerun after failing in the concurrent batch.
