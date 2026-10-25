// Another writer's clip lands while the editor's save is in flight, and the
// editor's re-read of it is held off by that save. The save keeps the clip,
// and once the save is back the editor reads again and shows it, so its next
// save starts from a base that has it.

const CLOUDS = "Clouds";
const STROBE = "Strobe";
fixture({
  seconds: 8,
  clips: [
    { pattern: "wash", name: "Wash", start: 0.5, end: 2 },
    { pattern: "haze", name: "Haze", start: 3, end: 5, preset: CLOUDS },
  ],
  outside_clips: [{ pattern: "agent", name: "Strobe", start: 5.5, end: 7, preset: STROBE }],
});

const card = (label) => (s) => s.find({ role: "card", label });

test("a clip written during a save survives it and reaches the editor", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the haze clip", card(CLOUDS));
  app.click(card(CLOUDS)(app.snapshot()));
  app.frames(2);
  app.key("delete");
  until("the haze clip gone", (s) => !card(CLOUDS)(s));
  until("the other writer's clip", card(STROBE));
  expect(Object.keys(library.score().clips).sort()).toEqual(["agent", "wash"]);
});
