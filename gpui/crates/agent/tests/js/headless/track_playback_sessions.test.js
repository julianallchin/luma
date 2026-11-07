// Returning to a parked song plays its own audio, however often the two
// songs trade places.

fixture({ seconds: 20, second_track_seconds: 8, equal_timestamp_track: true, seeded_threads: true });

test("returning to a parked song plays its own audio", () => {
  // The playhead only runs on a player reading that names the open song, so
  // a running playhead is that song's own audio. Opening the next song
  // while one plays lets the playing one go.
  const play = () => nav.play();
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
  play();
  nav.track("Zulu");
  waveform("Zulu");
  play();
  nav.track("Aurora");
  play();
  // Repeated switches exercise old poll loops as well as the cached tab.
  nav.track("Zulu");
  play();
  nav.track("Aurora");
  play();
  nav.pause();
});
