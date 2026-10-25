// The AI section's API key field against a real renderer. What the pixels are
// read for is that the field is masked: two different keys of one length must
// draw the same, which no amount of automation-tree reading can show.

fixture({ track: false, seconds: 1, window: [1100, 800] });

const FIELD = { role: "input", label: "OpenRouter API key" };
const CLEAR = { role: "button", label: "Clear OpenRouter API key" };

function openAiSettings() {
  app.action("luma::OpenSettings");
  nav.step("the AI settings section", "toggle", "AI");
  return until("the loaded key field", (s) => s.find(FIELD) !== undefined);
}

test("a typed key draws as a mask, and a saved one only as its hint", () => {
  nav.venue("Test Venue");
  openAiSettings();
  app.frames(4, { waitMs: 40 });
  image.keep(app.screenshot(), "settings/ai-no-key");

  const field = app.snapshot().find(FIELD);
  app.type(field, "sk-or-v1-aaaaaaaaaaaaaaaaaaaaaaaa-1111");
  const first = app.screenshot();
  app.key("escape");
  until("the dropped draft", (s) => s.find({ role: "button", label: "Save OpenRouter API key" }));
  app.type(app.snapshot().find(FIELD), "sk-or-v1-QWERTYUIOPASDFGHJKLZXCVB-wxyz");
  const second = app.screenshot();
  image.keep(second, "settings/ai-key-typed");
  const changed = image.diff(first, second, { rect: field.bounds });
  expect(changed).toBe(0);
  // …and the mask is ink, not an empty box.
  const ink = image.stats(second, field.bounds);
  assert(ink.max - ink.min > 40, `the typed key drew nothing: ${JSON.stringify(ink)}`);

  app.key("enter");
  until("the stored key", (s) => s.find(CLEAR));
  app.frames(4, { waitMs: 40 });
  image.keep(app.screenshot(), "settings/ai-key-set");
});
