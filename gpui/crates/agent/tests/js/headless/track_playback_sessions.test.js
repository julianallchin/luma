// Returning to a parked song plays its own audio, however often the two
// songs trade places.

fixture({ seconds: 20, second_track_seconds: 8, equal_timestamp_track: true, seeded_threads: true });

test("returning to a parked song plays its own audio", () => {
  const time = () => app.snapshot().findAll({ role: "text" }).find((n) => /^\d+:\d+ \/ \d+:\d+$/.test(n.label))?.label;
  // The player reads `elapsed / duration`; each song's duration is its own.
  function play(duration) {
    nav.step("a ready player", "button", "Play");
    until("playing the selected song", (s) => s.find({ role: "button", label: "Pause" }));
    app.frames(4, { waitMs: 60 });
    expect(time()).toMatch(` / ${duration}$`);
  }
  const waveform = (track) => until(`${track}'s waveform`, (s) => s.find({ role: "card", label: "Waveform" }));
  nav.venue("Test Venue");
  // Zulu is in the library but not in the venue: venue membership is
  // explicit, so it joins through the add-track dialog. The dialog's row is
  // the wide one; the venue list behind it is narrow.
  nav.step("the add-track affordance", "button", "Add track");
  const inDialog = (s) => s.findAll({ role: "row", label: "Zulu" }).find((row) => row.bounds.width > 300);
  app.click(inDialog(until("Zulu in the add-track dialog", inDialog)));
  until("Zulu in the venue", (s) =>
    s.find({ role: "text", label: "Zulu venue scores: 1" }) !== undefined &&
    s.find({ role: "button", label: "Close" }) === undefined);
  nav.track("Aurora");
  waveform("Aurora");
  play("0:20");
  nav.track("Zulu");
  waveform("Zulu");
  play("0:08");
  nav.track("Aurora");
  play("0:20");
  // Repeated switches exercise old poll loops as well as the cached tab.
  nav.track("Zulu");
  play("0:08");
  nav.track("Aurora");
  play("0:20");
  nav.step("pause", "button", "Pause");
  until("stopped", (s) => s.find({ role: "button", label: "Play" }));
});
