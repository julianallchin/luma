// The stage is lit by the score the timeline is on — the same one.
//
// A `(track, venue)` pair carries as many scores as people annotated it. The
// claim is a correspondence: whichever score the sidebar opens, the stage
// names that one, and keeps up when the choice changes. The stage's
// automation label is read because the fact under test is which document was
// installed; the readout is written when the install lands.

// Three scores to choose between, and a rig to make the composite worth
// installing.
fixture({ seconds: 8, clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }], extra_scores: 2, rig: 4 });

test("the stage is lit by the score the timeline opened", () => {
  nav.venue("Test Venue");
  nav.scores("Aurora");
  const rows = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
  // `Score #n` is the timeline's readout; `RIG SCORE #n` the stage's.
  const timeline = () => app.snapshot().findAll({ role: "text" }).find((n) => n.label.startsWith("Score #"))?.label;
  const rig = () => app.snapshot().findAll({ role: "card" }).find((n) => n.label.startsWith("RIG SCORE #"))?.label;
  const ordinal = (label) => label?.slice(label.indexOf("#"));

  const listed = rows().map((n) => n.label.split(" ")[0]);
  expect(listed.length).toBe(3);
  const open = (handle) => {
    app.click(rows().find((n) => n.label.split(" ")[0] === handle));
    until(`the timeline on ${handle}`, () => ordinal(timeline()) === handle);
    // The install is a round trip, so the stage's readout lands after the
    // timeline's; waiting for it is the point.
    until(`the rig lit by ${handle}`, () => ordinal(rig()) === handle);
    // A new tab puts the sidebar away a double-click interval after the
    // click; bring it back for the next pick.
    until("the sidebar put away", (s) => s.find({ role: "card", label: "Sidebar" }) === undefined);
    app.action("luma::ToggleSidebar");
    until("the scores again", () => rows().length === listed.length);
    return timeline();
  };
  const one = open(listed[0]);
  // …and onto a score the rig is not on. A stage that resolved the score
  // itself would sit here, unmoved.
  const two = open(listed[1]);
  assert(one !== two, "the two opens were the same score, so the switch proved nothing");
});
