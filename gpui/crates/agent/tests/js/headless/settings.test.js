// The settings screen, driven end to end against a disposable library: a
// click writes through the dispatch seam, and the next read of that seam is
// what the screen redraws from. A stubbed library would prove only that a
// label changed.

fixture({ track: false, seconds: 1 });

// Through ⌘, rather than the venue menu: these tests are about the seam.
// The menu's own door is the last test in this file.
function openAiSettings(model) {
  app.action("luma::OpenSettings");
  nav.step("the AI settings section", "toggle", "AI");
  return until("the loaded AI settings", (s) => s.find({ role: "select", label: model }) !== undefined);
}

const selects = (shot) => shot.findAll({ role: "select" }).map((n) => n.label);

// The second read is the one that matters: it is served by a fresh
// `get_settings`, so it can only say what the database says.
test("the model picker writes through the seam and reads back", () => {
  nav.venue("Test Venue");

  const opened = openAiSettings("Claude Opus 5");

  // Open the picker, then choose the option that is not selected. The pick is
  // a write plus a re-read on a runtime gpui does not own — waited for by its
  // result, not by a frame count.
  app.click(opened.find({ role: "select", label: "Claude Opus 5" }));
  app.click(app.snapshot().find({ role: "button", label: "Kimi K3 Fast" }));
  const picked = until("the picked model", (s) => s.find({ role: "select", label: "Kimi K3 Fast" }));
  // Provider is untouched — the gateway default — and the model is the pick.
  expect(selects(picked)).toEqual(["API", "Vercel AI Gateway", "Kimi K3 Fast"]);

  // Back to the venue shell settings were opened over, venue intact.
  nav.dismiss();
  const home = app.snapshot().nodes.map((n) => n.label);
  expect(home).toContain("Test Venue");
  assert(!home.includes("Settings"), "dismissing settings left them on screen");

  // …and in again, which re-reads the seam.
  expect(selects(openAiSettings("Kimi K3 Fast"))).toEqual(["API", "Vercel AI Gateway", "Kimi K3 Fast"]);

  app.click(app.snapshot().find({ role: "select", label: "API" }));
  app.click(app.snapshot().find({ role: "button", label: "Codex subscription" }));
  until("the subscription engine", (s) => s.find({ role: "select", label: "Codex subscription" }));
  nav.dismiss();
  expect(selects(openAiSettings("Codex subscription"))).toEqual(["Codex subscription"]);
});

// An API key goes in through the masked field and is stored where the agent
// reads it; the screen never shows it back. A reopen re-reads the seam, so
// "Clear" surviving it is the database saying a key is stored.
test("an API key is saved and cleared without being shown", () => {
  const SECRET = "sk-or-v1-harness-0123456789-wxyz";
  const field = { role: "input", label: "OpenRouter API key" };
  const clear = { role: "button", label: "Clear OpenRouter API key" };
  const leaked = (shot) => shot.nodes.some((n) => (n.label || "").includes("0123456789"));
  nav.venue("Test Venue");

  const opened = openAiSettings("Claude Opus 5");
  assert(opened.find(clear) === undefined, "a fresh library already has a key");
  app.type(opened.find(field), SECRET);
  assert(!leaked(app.snapshot()), "the typed key is in the automation tree");
  app.key("enter");
  const saved = until("the stored key", (s) => s.find(clear));
  assert(!leaked(saved), "the stored key is in the automation tree");

  nav.dismiss();
  const reopened = openAiSettings("Claude Opus 5");
  assert(reopened.find(clear) !== undefined, "the key did not survive a re-read");
  assert(!leaked(reopened), "the re-read key is in the automation tree");
  // The gateway, which was never given a key, still has none.
  assert(reopened.find({ role: "button", label: "Clear Vercel AI Gateway API key" }) === undefined,
    "a key written for one service shows on another");

  app.click(reopened.find(clear));
  until("the cleared key", (s) => s.find(field) && s.find(clear) === undefined);
  nav.dismiss();
  assert(openAiSettings("Claude Opus 5").find(clear) === undefined, "the cleared key came back");
});

// The HDR output row on General: present, and a press writes through the seam
// without a save error. The automation tree carries no checked state, and the
// harness has no HDR display, so what the window does with it is not visible.
test("HDR output is a General setting", () => {
  nav.venue("Test Venue");
  // The ⌘, door: this test is about the row, not the account menu.
  app.action("luma::OpenSettings");
  const row = until("the loaded General settings", (s) =>
    s.find({ role: "checkbox", label: "HDR output" }));
  app.click(row.find({ role: "checkbox", label: "HDR output" }));
  // The write re-reads every setting; a failed write shows as a text node.
  const settled = until("the re-read after the write", (s) =>
    s.find({ role: "checkbox", label: "HDR output" }));
  expect(settled.findAll({ role: "checkbox" }).map((n) => n.label)).toContain("HDR output");
  const failed = settled.nodes.some((n) => (n.label || "").startsWith("Failed to save"));
  assert(!failed, "the HDR write reported a save error");
});

// The door a person can see: the title bar's venue menu, then its Settings row.
test("the venue menu opens settings", () => {
  nav.venue("Test Venue");
  nav.settings();
  until("the settings screen", (s) => s.find({ role: "toggle", label: "AI" }));
});

// Settings is a page in place of the page's whole content, not a dialog over
// it: the title bar keeps only the window's corner and Back, and the agent
// panel is not slid away or back — it is simply not drawn, then drawn again
// where it was.
const settingsPage = (s) => s.find({ role: "card", label: "Settings" });
const agent = (s) => s.find({ role: "card", label: "Agent" })?.bounds;
// Every frame drawn since `frame`, oldest first.
const since = (frame) => app.painted().filter((s) => s.frame >= frame);

test("settings takes the page in one frame, and gives it back the same way", { fixture: { track: true, seconds: 4 } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  const before = until("the agent panel at rest", (s) => agent(s)?.width > 0 ? s : undefined);
  app.frames(4);
  const rest = app.snapshot();
  const panel = agent(rest);
  const timeline = rest.find({ role: "card", label: "Waveform" }).bounds;
  const mode = nav.shownMode(before);

  const { frame: opened } = app.action("luma::OpenSettings");
  const shot = until("the settings page", (s) =>
    settingsPage(s) && s.find({ role: "toggle", label: "General" }) ? s : undefined);
  // From the first frame it is up, the agent panel is gone, not leaving.
  for (const s of since(opened).filter(settingsPage)) {
    expect(agent(s)).toBe(undefined);
  }
  const page = shot.find({ role: "card", label: "Page" }).bounds;
  const settings = settingsPage(shot).bounds;
  const bar = shot.find({ role: "card", label: "Title bar" }).bounds;
  expect(shot.find({ role: "button", label: "Dismiss dialog" })).toBe(undefined);
  expect(shot.find({ role: "card", label: "Waveform" })).toBe(undefined);
  assert(settings.y >= bar.y + bar.height - 1, "the settings page covers the title bar");
  assert(Math.abs(settings.width - page.width) <= 1 && Math.abs(settings.x - page.x) <= 1,
    `the settings page does not span the page: ${JSON.stringify({ settings, page })}`);
  // The title bar holds Back and nothing else of ours: no venue menu, no
  // agent toggle, no modes.
  const back = shot.find({ role: "button", label: "Back" }).bounds;
  assert(back.y >= bar.y && back.y + back.height <= bar.y + bar.height, "Back is not in the title bar");
  expect(shot.find({ role: "card", label: "Modes" })).toBe(undefined);
  expect(shot.find({ role: "button", label: "Test Venue" })).toBe(undefined);
  expect(shot.find({ role: "button", label: "Hide agent" })).toBe(undefined);
  expect(shot.find({ role: "button", label: "Show agent" })).toBe(undefined);

  // Back brings the page back as it was, the panel at its width from the
  // first frame: no slide in.
  const { frame: closed } = app.click(shot.find({ role: "button", label: "Back" }));
  const back_ = until("the score again", (s) => !settingsPage(s) && agent(s) ? s : undefined);
  for (const s of since(closed).filter((s) => !settingsPage(s))) {
    expect(agent(s)).toEqual(panel);
  }
  expect(nav.shownMode(back_)).toBe(mode);
  expect(back_.find({ role: "card", label: "Waveform" }).bounds).toEqual(timeline);
  expect(back_.find({ role: "card", label: "Modes" }) !== undefined).toBe(true);
});

test("Escape leaves the settings page for where you were, keyboard and all", () => {
  nav.venue("Test Venue");
  nav.songs();
  const search = (s) => s.find({ role: "input", label: "Search tracks" });
  app.click(search(app.snapshot()));
  expect(search(app.snapshot()).focused).toBe(true);
  app.action("luma::OpenSettings");
  until("the settings page", (s) => settingsPage(s) && !search(s));
  app.key("escape");
  const back = until("the song list again", (s) => !settingsPage(s) && search(s) ? s : undefined);
  expect(search(back).focused).toBe(true);
});

test("Back leaves the settings page for the mode it was opened over", () => {
  nav.venue("Test Venue");
  nav.venuePage("Test Venue");
  app.action("luma::OpenSettings");
  until("the settings page", settingsPage);
  // The mode keys are the shell's, and the page owns the keyboard.
  app.key("3");
  app.frames(2);
  assert(settingsPage(app.snapshot()), "a mode key left the settings page");
  nav.step("Back", "button", "Back");
  until("the venue page again", (s) =>
    !settingsPage(s) && s.find({ role: "card", label: "Test Venue Venue" }));
});

// Back takes the agent toggle's place: one gap past the window's corner,
// with no room held for the anchors the page puts away.
test("Back sits right after the window controls", () => {
  nav.venue("Test Venue");
  const agentToggle = (s) =>
    s.find({ role: "button", label: "Hide agent" }) ?? s.find({ role: "button", label: "Show agent" });
  const shell = until("the agent toggle", (s) => agentToggle(s) ? s : undefined);
  const corner = shell.find({ role: "card", label: "Window controls" }).bounds;
  const toggle = agentToggle(shell).bounds;
  const end = corner.x + corner.width;
  app.action("luma::OpenSettings");
  const shot = until("the settings page", (s) =>
    settingsPage(s) && s.find({ role: "button", label: "Back" }) ? s : undefined);
  const back = shot.find({ role: "button", label: "Back" }).bounds;
  assert(back.x >= end, `Back overlaps the window controls: ${JSON.stringify({ back, corner })}`);
  assert(back.x - end <= toggle.x - end + 1,
    `Back sits further from the corner than the first control does: ${JSON.stringify({ back, toggle, corner })}`);
});

// The search looks through every section, not only the one showing.
const searchField = (s) => s.find({ role: "input", label: "Search settings" });
const hdr = (s) => s.find({ role: "checkbox", label: "HDR output" });
const clearField = () => app.key("secondary-a backspace");

test("searching shows a row from another section, and clearing restores the page", () => {
  nav.venue("Test Venue");
  const opened = openAiSettings("Claude Opus 5");
  assert(hdr(opened) === undefined, "the AI section already shows a General row");
  app.type(searchField(opened), "hdr");
  const found = until("the HDR row from General", (s) => hdr(s) ? s : undefined);
  // Only what matched: the AI section's own selects and the nav's sections
  // step aside.
  expect(selects(found)).toEqual([]);
  expect(found.find({ role: "toggle", label: "AI" })).toBe(undefined);

  // Help text is searched too: no label says "silent".
  clearField();
  app.type(searchField(until("the emptied search", searchField)), "silent");
  until("the audio row, by its help", (s) =>
    s.find({ role: "checkbox", label: "Enable Audio Output" }) && !hdr(s));

  clearField();
  const restored = until("the AI section again", (s) =>
    searchField(s) && s.find({ role: "toggle", label: "AI" }) ? s : undefined);
  expect(selects(restored)).toEqual(["API", "Vercel AI Gateway", "Claude Opus 5"]);
  expect(hdr(restored)).toBe(undefined);
});

test("Escape empties the search before it leaves settings", () => {
  nav.venue("Test Venue");
  app.action("luma::OpenSettings");
  const opened = until("the settings page", (s) => settingsPage(s) && searchField(s) ? s : undefined);
  app.type(searchField(opened), "nothing says this");
  until("the empty result", (s) => s.find({ role: "button", label: "Clear search" }));
  app.key("escape");
  const cleared = until("the page again", (s) =>
    searchField(s) && s.find({ role: "toggle", label: "General" }) ? s : undefined);
  assert(settingsPage(cleared), "the first escape left settings");
  app.key("escape");
  until("the shell again", (s) => !settingsPage(s));
});
