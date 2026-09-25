// The settings screen, driven end to end against a disposable library: a
// click writes through the dispatch seam, and the next read of that seam is
// what the screen redraws from. A stubbed library would prove only that a
// label changed.

fixture({ track: false, seconds: 1 });

// Through ⌘, rather than the account foot: these tests are about the seam.
// The foot's own door is the last test in this file.
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

// The door a person can see: the account foot's menu, then its Settings row.
test("the account foot opens settings", () => {
  nav.venue("Test Venue");
  nav.settings();
  until("the settings screen", (s) => s.find({ role: "toggle", label: "AI" }));
});
