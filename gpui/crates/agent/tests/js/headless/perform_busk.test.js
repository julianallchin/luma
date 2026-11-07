// Setup's "Busk page" makes a page from the venue's rig. The venue has an
// empty Perform set and two dimmer-only movers, in two groups, left_movers
// and right_movers: nothing aims, so the page has no movement row.

fixture({ seconds: 1, rig: 2, window: [1400, 1100] });

test("Busk page makes a page from the rig, its rows top to bottom", () => {
  nav.venue("Test Venue");
  nav.live();
  // "Edit layout" does nothing until the layout has landed.
  until("the set's first page", (s) => s.find({ role: "toggle", label: "Page Main" }));
  nav.step("Edit layout", "button", "Edit layout");
  until("the empty set's first page", (s) => s.find({ role: "button", label: "Select Master" }));
  nav.step("a busk page", "button", "Add busk page");

  const shot = until("the busk page shown", (s) =>
    s.find({ role: "toggle", label: "Page Busk" })?.focused &&
    s.find({ role: "button", label: "Select Black" })
      ? s
      : undefined,
  );
  const top = (label) => {
    const node = shot.find({ role: "button", label: `Select ${label}` });
    expect(node).toBeTruthy();
    return node.bounds.y;
  };
  // Groups to choose, everything and the rig's, side by side.
  expect(top("Left movers")).toBe(top("All"));
  expect(top("Right movers")).toBe(top("All"));
  const rows = ["All", "Base color", "Accent color", "Chase", "Strobe"].map(top);
  for (let at = 1; at < rows.length; at++) {
    expect(rows[at]).toBeGreaterThan(rows[at - 1]);
  }
  expect(top("Black")).toBe(top("Strobe"));
  expect(shot.find({ role: "button", label: "Select Sweep" })).toBe(undefined);
  expect(shot.find({ role: "button", label: "Select Speed" })).toBeTruthy();
  // Select and Clear head the side column.
  expect(top("Select")).toBe(top("All"));
  expect(top("Clear")).toBeGreaterThan(top("Select"));
  // The first page is still there.
  expect(shot.find({ role: "toggle", label: "Page Main" })).toBeTruthy();
});
