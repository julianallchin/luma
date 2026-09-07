# Venue workspace — native revision

The venue lives in the right workspace column, next to chat. It starts at 65% of the shared width, with a 360 px minimum for chat and a draggable divider. A live stage preview sits above compact editing controls. The tone is professional: short labels, direct manipulation and contextual controls, without tutorial copy. It must remain useful at the shell's 320 px workspace minimum.

## Implemented

- One Venue tab: live stage above, groups and fixture inventory below. There are no Stage / Fixtures / Groups navigation tabs.
- Add element and Objects live on the preview toolbar. Objects is a flat popover for selecting structures and placing unplaced inventory; it uses the same selection as the viewport. Placement, snapping, distribution, duplicate, flip and undo retain their existing behavior.
- The lower panel shows groups and fixtures together. Selecting a fixture opens its label, mode, universe, address, authored group membership, and geometry controls above the list. Unplaced fixtures offer Place. Add fixtures uses the existing fixture picker and allocator. Viewport selection and inventory selection stay in sync; Shift or the platform modifier extends selection.
- Selected elements expose Duplicate and Remove. Removal uses the native confirmation dialog. Removing structure preserves attached fixtures as unplaced inventory, and undo restores the structure; deleting a fixture directly still removes its patch row. Native palettes can use more window width when opened.
- View opens render settings in every visualizer, including score previews. It contains Indoor/Outdoor, House/Sun, and the advanced Renderer Lab. Environment values remain saved with the venue and do not change when a score starts. Scrubs update immediately and serialize their writes; the final value is still saved if navigation closes the preview.
- Group chips show authored groups first, then automatic groups marked auto. Clicking opens a draft editor for its name, parent and members. Membership edits also update the stage highlight. Parent paths distinguish same-named leaves without an expanded tree. The same fixture can belong to multiple authored groups. Parent changes organize groups; moving physical structures remains a stage operation.
- A group save is one backend transaction. A missing fixture, name conflict or permission failure rolls back the whole edit. Cancelling discards the draft.
- Patch details replaces only the lower region with the technical table, footprint, auto patch and local output routing. Closing it returns to the inventory. The table remains horizontally scrollable.
- No schema migration or rendering changes. Ambient occlusion remains future renderer work.

## Important limits / next pass

Group selector names are currently the strings saved effects reference. The native save command and existing rename commands refuse to change a name mentioned in saved work. Membership remains editable. The reference check is deliberately conservative and also protects names found in graph labels or legacy implementations. The complete solution is stable group selector identity or an authored-history transaction updating every affected score, rather than silently making an existing effect select nothing.

The first membership editor operates on whole fixtures. Existing per-head memberships are retained unless their fixture is explicitly removed, but individual heads still need their own editor and partial-membership indicator. Derived/merged groups need clearer membership provenance before exposing arbitrary membership changes.

Bulk patch editing is still the wide table behind Patch details. Per-fixture edits are available directly in the compact inspector. Geometry controls can be dense for fixtures in distributed rows; they retain the existing builder vocabulary.

The original concept image in this directory was generated. The native captures are actual GPUI windows using the user's EBF library on a dedicated X11 display; they are not web mockups. No in-app agent/model turns were used.

## Validation

- Native group workflow at 1100 × 900: create, assign, edit, rename, reparent and cancel without changing pages.
- Inline fixture edits and group membership match the values in Patch details. Render settings follow a score and survive closing/reopening the venue preview.
- Existing patch tests cover address refusal, occupancy, outputs, auto patch, destructive confirmation, adding fixtures and mode changes.
- Existing builder tests cover placement, attachments, distribution, fit refusal, duplicate/flip, detach, palette focus and undo/redo. The duplicate test now uses the platform's secondary modifier instead of hard-coding macOS Command.
- Backend tests cover atomic rollback, rename with membership changes, generated-group reparenting, cycle refusal and protecting names used by saved scores. Native tests cover element selection, confirmed removal, retained fixture inventory and undo, plus the wider workspace split and its resizing limits.
- Workspace/all-targets check and native application build.

Current validation: all 15 builder tests, all 10 patch/group/render-settings tests, and the venue-filter browser test pass (26 total). The workspace/all-targets check and native build pass. Native X11 captures were inspected using the actual EBF library with chat visible, including fixture selection, Objects and View. No venue contents were changed for captures.

The previous revision's additional venue-picker stale-response test failed while reopening its catalogue (`venue_launch_picker_create_and_stale_reads_are_correlated`); its injected catalogue failure arrives earlier than the test expects. That separate picker/request-order issue is outside this layout change. The track-list ordering test passed on an isolated rerun after failing in the concurrent batch.
