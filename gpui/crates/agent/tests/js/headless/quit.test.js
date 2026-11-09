// Luma has one window, so closing it quits, and ⌘Q is the same request. With
// nothing waiting to upload the window just closes. With local changes not
// yet on the server it stays, and a dialog says so: Quit Now leaves anyway,
// Cancel stays. Offline, nothing would drain, so the dialog says the changes
// sync next time and offers Quit.
//
// The drain itself (the dialog quitting on its own when the queue reaches
// zero) needs the status changed mid-run, which only a Rust fixture can do:
// see `tests/headless/quit_sync.rs`.

fixture({});

const status = (fields) => ({
  connected: true,
  uploading: false,
  downloading: false,
  lastSyncedAt: null,
  pendingUploads: 0,
  refused: [],
  ...fields,
});
const dialog = (s) => s.find({ role: "card", label: "Quit sync dialog" });
const closed = () => until("the window to close", () => !app.isOpen());

test("closing the window with no sync quits", () => {
  nav.trackEditor("Test Venue", "Aurora");
  app.close();
  closed();
});

test("closing the window with nothing to upload quits", { fixture: { sync_status: status({}) } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  app.close();
  closed();
});

test("⌘Q with nothing to upload quits", { fixture: { sync_status: status({}) } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  app.key("cmd-q");
  closed();
});

test("⌘Q with changes uploading asks, and Quit Now quits", {
  fixture: { sync_status: status({ uploading: true, pendingUploads: 3 }) },
}, () => {
  nav.trackEditor("Test Venue", "Aurora");
  app.key("cmd-q");
  const s = until("the dialog", (s) => dialog(s) && s);
  assert(s.find({ role: "text", label: "Syncing your changes" }), "the dialog does not say it is syncing");
  assert(s.find({ role: "text", label: "3 changes left. Luma quits when they are uploaded." }), "no count");
  assert(app.isOpen(), "quit before the changes were up");
  app.click(s.find({ role: "button", label: "Quit Now" }));
  closed();
});

test("the close button asks the same, and Cancel stays", {
  fixture: { sync_status: status({ uploading: true, pendingUploads: 2 }) },
}, () => {
  nav.trackEditor("Test Venue", "Aurora");
  app.close();
  const s = until("the dialog", (s) => dialog(s) && s);
  app.click(s.find({ role: "button", label: "Cancel" }));
  until("the dialog gone", (s) => !dialog(s));
  assert(app.isOpen(), "Cancel quit");
  // Asking again asks again: nothing about the first answer stuck.
  app.key("cmd-w");
  until("the dialog again", (s) => dialog(s));
});

test("offline, the changes wait for next time and Quit quits", {
  fixture: { sync_status: status({ connected: false, pendingUploads: 2 }) },
}, () => {
  nav.trackEditor("Test Venue", "Aurora");
  app.key("cmd-q");
  const s = until("the dialog", (s) => dialog(s) && s);
  assert(s.find({ role: "text", label: "2 changes will sync the next time you open Luma." }), "no promise");
  assert(!s.find({ role: "button", label: "Quit Now" }), "offline offers to wait");
  app.click(s.find({ role: "button", label: "Quit" }));
  closed();
});
