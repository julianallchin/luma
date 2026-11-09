// Settings → About → Update channel: choosing a channel checks it at once,
// against releases the fixture serves, and the choice is stored. The app is
// stable 0.6.3; stable publishes that same build, nightly a release that
// sorts below it and has no build for this machine. So "up to date" can only
// come from the stable manifest, and a failed check can only come from the
// nightly one being offered: a switch offers the other channel's release
// whatever its version.

fixture({
  track: false,
  seconds: 1,
  update_check: {
    channel: "stable",
    version: "0.6.3",
    commit: "aaaaaaa",
    manifests: {
      stable: { version: "0.6.3", commit: "aaaaaaa", platforms: {} },
      nightly: { version: "0.6.2-nightly.20261108+bbbbbbb", commit: "bbbbbbb", platforms: {} },
    },
  },
});

const BUILD = { role: "text", label: "Luma 0.6.3 · Stable" };
const FAILED = { role: "text", label: "Could not check for updates." };
const UP_TO_DATE = { role: "text", label: "Luma is up to date." };

const stored = () =>
  library.query("SELECT value FROM settings WHERE key = 'update_channel'").map((row) => row.value);

function openAbout() {
  app.action("luma::OpenSettings");
  nav.step("the About section", "toggle", "About");
  return until("the update channel", (s) => s.find({ role: "toggle", label: "Nightly" }));
}

test("a switch checks the chosen channel now, and the choice is stored", () => {
  nav.venue("Test Venue");
  const opened = openAbout();
  assert(opened.find(BUILD), "About does not name the running build");
  // A source build never checks by itself; nothing is chosen yet.
  assert(!opened.find(FAILED) && !opened.find(UP_TO_DATE), "a check ran before any switch");
  expect(stored()).toEqual([]);

  app.click(opened.find({ role: "toggle", label: "Nightly" }));
  until("the nightly check to fail on a release with no build here", (s) => s.find(FAILED));
  expect(stored()).toEqual(["nightly"]);

  app.click(app.snapshot().find({ role: "toggle", label: "Stable" }));
  const back = until("the stable check", (s) => s.find(UP_TO_DATE));
  expect(stored()).toEqual(["stable"]);
  // The running build is still what it was: a switch installs, never relabels.
  assert(back.find(BUILD), "the switch relabelled the running build");
});
