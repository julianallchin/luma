// The app's keyboard, end to end: a key, an action, and a text field that
// keeps the keys it is typing.
//
// *Routing*: an action only reaches a handler if something on screen holds
// focus, so space has to move the transport and not merely be swallowed.
// *Naming*: `app.action("luma::PlayPause")` and the space bar must arrive at
// the same place. *Precedence*: gpui matches key bindings before it delivers
// key events, so a focused text field cannot defend its own space bar — only
// the binding's `!TextInput` scope can.
//
// Headless has no sound, so the evidence that space started the transport is
// that the playhead moved on its own between two readings.

fixture({ seconds: 20, clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }] });

function read() {
  const shot = app.snapshot();
  const search = shot.find({ role: "input" });
  return {
    playhead: shot.find({ role: "slider", label: "Playhead" })?.bounds.x ?? null,
    buttons: shot.findAll({ role: "button" }).map((n) => n.label),
    text: shot.findAll({ role: "text" }).map((n) => n.label),
    search: search === undefined ? null : { label: search.label, focused: search.focused },
    // The venue picker draws a card per venue.
    onVenuePicker: shot.find({ role: "card", label: "Test Venue" }) !== undefined,
  };
}

// Arrange shows the transport only as its playhead: running or held.
const running = () => until("the playhead running", () => nav.moving());
const held = () => until("the playhead held", () => !nav.moving());

function openEditor() {
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.agentOff();
  return read();
}

test("keys and actions route to the focused screen and a text field keeps its own", () => {
  // 0. Before any click, onboarding owns the modal focus scope and has
  //    focused its first control. An app-wide shortcut must not replace the
  //    required first-venue dialog.
  app.key("secondary-,");
  app.frames(6);
  const cold = read();
  assert(cold.onVenuePicker && cold.search?.focused === true && !cold.text.includes("Settings"),
    `required onboarding did not keep its focused modal scope: ${JSON.stringify(cold)}`);
  app.key("escape");
  app.frames(6);

  nav.venue("Test Venue");
  app.frames(8);
  const opened = openEditor();
  expect(nav.moving()).toBe(false);

  // 1. Space plays. Nothing was clicked, so this is focus routing a binding.
  //    The player loads behind the timeline, so start it by name first and
  //    stop it, which proves it is ready for the key.
  nav.play();
  nav.pause();
  const before = read().playhead;
  app.key("space");
  running();
  expect(read().playhead).toBeGreaterThan(before);

  // 2. Space again stops it, and a stopped playhead stays put.
  app.key("space");
  held();
  const paused = read();
  app.frames(6, { waitMs: 60 });
  expect(read().playhead).toBe(paused.playhead);

  // 3. The same verb by name.
  app.action("luma::PlayPause");
  running();
  app.key("space");
  held();

  // 4. With nothing to back out of, Escape leaves the editor be; ⌘P is the
  //    way to the song list.
  app.key("escape");
  app.frames(6);
  const stayed = read();
  assert(!stayed.onVenuePicker && stayed.search === null && stayed.playhead !== null,
    `escape left the editor: ${JSON.stringify(stayed.search)}`);
  app.key("secondary-p");
  app.frames(6);
  const back = read();
  assert(!back.onVenuePicker, "⌘P went further back than the song list");
  assert(back.search !== null, "⌘P did not land on the song list");

  // 5. The search field takes the keyboard and keeps it: space is a space,
  //    escape clears the query rather than leaving the venue.
  app.click(app.snapshot().find({ role: "input" }));
  app.frames(2);
  expect(read().search.focused).toBe(true);
  app.key("space");
  app.frames(2);
  const typed = read();
  expect(typed.search.label).toBe(" ");
  app.key("escape");
  app.frames(2);
  const cleared = read();
  expect(cleared.search.label).toBe("Search tracks");
  assert(!typed.onVenuePicker && !cleared.onVenuePicker, "a key meant for the search field navigated instead");

  // 6. Nothing the search field was sent reached the transport.
  openEditor();
  expect(nav.moving()).toBe(false);

  // 7. ⌘, over whatever is showing, and out again.
  app.key("secondary-,");
  app.frames(6);
  expect(read().text).toContain("Settings");
  app.key("escape");
  app.frames(6);
  expect(read().text).toContain("Aurora");
});
