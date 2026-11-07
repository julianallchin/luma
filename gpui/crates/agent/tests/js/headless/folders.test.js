// The song list's folders, end to end.
//
// - "New folder" makes a folder and puts the caret in its name;
// - a right-click on a song lists the folders with checkboxes, and ticking
//   one puts the song in it;
// - a selected folder lists its songs, and each is the same song as under All
//   songs: it opens the same scores;
// - deleting a folder that holds songs asks first, and takes only the folder.

fixture({ clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }] });

const folderRow = (s, name) => s.find({ role: "row", label: `Folder ${name}` });
const songsIn = (s, name) =>
  s.findAll({ role: "text" }).find((n) => n.label.startsWith(`Folder ${name} songs: `))?.label;
const songRows = (s) => s.findAll({ role: "row", label: "Aurora" });
const dialog = (s) => s.find({ role: "card", label: "Confirm dialog" });
const scoresShown = (s) => s.find({ role: "card", label: "Scores of Aurora" }) !== undefined;
const scoreRows = (s) => s.findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));

test("a folder is made, named, filled from a song's menu, opened and deleted", () => {
  nav.venue("Test Venue");
  until("the song", (s) => songRows(s).length === 1);

  // Made, and named in place.
  nav.step("the new folder action", "button", "New folder");
  const field = () => app.snapshot().find({ role: "input", label: "Folder name" });
  until("the name field", () => field() !== undefined);
  app.key("ctrl-a backspace");
  app.type(field(), "Warm up");
  app.key("enter");
  const made = until("the named folder", (s) => folderRow(s, "Warm up"));
  expect(songsIn(made, "Warm up")).toBe("Folder Warm up songs: 0");
  expect(made.find({ role: "row", label: "Folder New folder" })).toBe(undefined);

  // An empty folder, selected, lists nothing and says so.
  app.click(folderRow(made, "Warm up"));
  until("the empty folder", (s) => songRows(s).length === 0
    && texts(s).includes("No songs in this folder"));
  nav.step("All songs", "row", "All songs");

  // Filled from the song's own menu.
  app.click(until("the song again", (s) => songRows(s).length === 1 ? s : undefined)
    .find({ role: "row", label: "Aurora" }), { button: "right" });
  const menu = until("the song's folders", (s) => s.find({ role: "card", label: "Song folders" }));
  app.click(menu.find({ role: "checkbox", label: "Warm up" }));
  until("the song in the folder", (s) => songsIn(s, "Warm up") === "Folder Warm up songs: 1");

  // A press outside closes the menu; then the folder lists its song, which
  // opens the same scores it does under All songs.
  app.click(app.snapshot().find({ role: "input", label: "Search tracks" }));
  until("the menu gone", (s) => s.find({ role: "card", label: "Song folders" }) === undefined);
  nav.step("All songs", "row", "All songs");
  nav.step("the song under All songs", "row", "Aurora");
  const all = until("its scores under All songs", (s) => scoresShown(s) && scoreRows(s).length > 0 ? s : undefined);
  const listed = scoreRows(all).map((n) => n.label);
  app.click(folderRow(app.snapshot(), "Warm up"));
  const filed = until("the folder's song", (s) => songRows(s).length === 1 && !scoresShown(s) ? s : undefined);
  app.click(songRows(filed)[0]);
  const level = until("the song's scores", (s) => scoresShown(s) && scoreRows(s).length > 0 ? s : undefined);
  expect(scoreRows(level).map((n) => n.label)).toEqual(listed);
  expect(scoreRows(level).length).toBe(1);

  // Deleting asks, because the folder holds a song, and takes only the
  // folder: the song stays under All songs, which the selection falls back to.
  app.click(folderRow(app.snapshot(), "Warm up"), { button: "right" });
  const actions = until("the folder menu", (s) => s.find({ role: "card", label: "Context menu" }));
  app.click(actions.find({ role: "button", label: "Delete folder" }));
  const asked = until("the confirmation", (s) => dialog(s));
  expect(asked.findAll({ role: "text" }).map((n) => n.label).join(" | ")).toContain("1 song");
  app.click(asked.findAll({ role: "button", label: "Delete folder" }).at(-1));
  const gone = until("the folder gone", (s) => folderRow(s, "Warm up") === undefined ? s : undefined);
  expect(until("the song under All songs", (s) => songRows(s).length === 1 ? s : undefined)
    .find({ role: "row", label: "Aurora" }) !== undefined).toBe(true);
  expect(texts(gone)).toContain("All songs songs: 1");
});
