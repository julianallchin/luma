//! Every MIDI input port, opened as it appears and dropped as it goes, and
//! the output of each device that has one, for its lights.
//!
//! One thread watches the ports for the life of the process, and a
//! [`Listener`] only subscribes to it. CoreMIDI tells a process about ports
//! coming and going on the run loop of the thread that made the process's
//! first client, and nowhere else. A thread that sleeps between scans sees
//! the ports present when it started and never another; once that thread
//! ends, no thread in the process sees a change again. So the watching
//! thread makes every client, runs its run loop between scans, and never
//! ends.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::Duration;

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use super::{Address, Event};

const CLIENT: &str = "Luma";
/// How often to look for ports plugged in or pulled out.
const SCAN_EVERY: Duration = Duration::from_secs(1);

/// Who hears the ports: the latest listener, while it is open.
static SUBSCRIBER: Mutex<Option<Subscriber>> = Mutex::new(None);

struct Subscriber {
    id: u64,
    sink: Box<dyn Fn(Event) + Send>,
    ports: Box<dyn Fn(Vec<String>) + Send>,
}

fn subscriber() -> MutexGuard<'static, Option<Subscriber>> {
    SUBSCRIBER.lock().expect("MIDI subscriber poisoned")
}

/// The output of each open input port that has one, by the port's name: a
/// device's input and output share it. Our own input ports are outputs to
/// the system too, and never inputs, so a light never loops back as a press.
static OUTPUTS: Mutex<BTreeMap<String, MidiOutputConnection>> = Mutex::new(BTreeMap::new());

fn outputs() -> MutexGuard<'static, BTreeMap<String, MidiOutputConnection>> {
    OUTPUTS.lock().expect("MIDI outputs poisoned")
}

/// Send `message` to the output of port `port`. Answers whether it went.
pub(super) fn send(port: &str, message: &[u8]) -> bool {
    match outputs().get_mut(port) {
        Some(output) => output.send(message).is_ok(),
        None => false,
    }
}

/// A subscription to the ports. Dropping it closes every port.
pub(super) struct Listener {
    id: u64,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let mut subscriber = subscriber();
        if subscriber.as_ref().is_some_and(|s| s.id == self.id) {
            *subscriber = None;
        }
    }
}

/// One MIDI message as a control input: a CC's value, or a note's velocity
/// (0 on release), both 0–1.
pub(super) fn parse(data: &[u8]) -> Option<Event> {
    let (&status, rest) = data.split_first()?;
    let channel = status & 0x0f;
    let (address, value) = match (status & 0xf0, rest) {
        (0xb0, [number, value, ..]) => (
            Address::MidiCc {
                channel,
                number: *number,
            },
            *value,
        ),
        (0x90, [number, velocity, ..]) => (
            Address::MidiNote {
                channel,
                number: *number,
            },
            *velocity,
        ),
        (0x80, [number, ..]) => (
            Address::MidiNote {
                channel,
                number: *number,
            },
            0,
        ),
        _ => return None,
    };
    Some(Event {
        address,
        value: f64::from(value) / 127.0,
        port: None,
    })
}

/// Open every port, now and as they come, handing each message to `sink`
/// and each change of the open ports to `ports`, until the listener drops.
/// A newer listener takes the ports from an older one.
pub(super) fn listen(
    sink: impl Fn(Event) + Send + 'static,
    ports: impl Fn(Vec<String>) + Send + 'static,
) -> Listener {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    static WATCH: Once = Once::new();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    *subscriber() = Some(Subscriber {
        id,
        sink: Box::new(sink),
        ports: Box::new(ports),
    });
    WATCH.call_once(|| {
        std::thread::Builder::new()
            .name("luma-midi-inputs".into())
            .spawn(watch)
            .expect("spawn the MIDI input thread");
    });
    Listener { id }
}

/// The watching thread: open the ports for whoever subscribes, forever.
fn watch() {
    let mut scanner: Option<MidiInput> = None;
    let mut out_scanner: Option<MidiOutput> = None;
    let mut open: BTreeMap<String, MidiInputConnection<()>> = BTreeMap::new();
    let mut serving: Option<u64> = None;
    let mut reported: Option<Vec<String>> = None;
    loop {
        // Read without holding the lock: closing a port waits for its
        // callback, which takes the lock.
        let id = subscriber().as_ref().map(|s| s.id);
        if id != serving {
            open.clear();
            outputs().clear();
            reported = None;
            serving = id;
        }
        if let Some(id) = serving {
            if scanner.is_none() {
                scanner = MidiInput::new(CLIENT).ok();
            }
            if let Some(scanner) = &scanner {
                scan(scanner, &mut open, id);
            }
            let names: Vec<String> = open.keys().cloned().collect();
            if out_scanner.is_none() {
                out_scanner = MidiOutput::new(CLIENT).ok();
            }
            if let Some(out_scanner) = &out_scanner {
                scan_outputs(out_scanner, &names);
            }
            if reported.as_ref() != Some(&names) {
                if let Some(s) = subscriber().as_ref().filter(|s| s.id == id) {
                    (s.ports)(names.clone());
                }
                reported = Some(names);
            }
        }
        wait(SCAN_EVERY);
    }
}

/// Drop the ports that went and open the ones that came, for subscriber `id`.
fn scan(scanner: &MidiInput, open: &mut BTreeMap<String, MidiInputConnection<()>>, id: u64) {
    let present: Vec<(String, midir::MidiInputPort)> = scanner
        .ports()
        .into_iter()
        .filter_map(|port| Some((scanner.port_name(&port).ok()?, port)))
        .collect();
    open.retain(|name, _| present.iter().any(|(known, _)| known == name));
    for (name, port) in present {
        if open.contains_key(&name) {
            continue;
        }
        let Ok(input) = MidiInput::new(CLIENT) else {
            continue;
        };
        let port_name = name.clone();
        let heard = move |_: u64, data: &[u8], _: &mut ()| {
            if let Some(event) = parse(data) {
                if let Some(s) = subscriber().as_ref().filter(|s| s.id == id) {
                    (s.sink)(Event {
                        port: Some(port_name.clone()),
                        ..event
                    });
                }
            }
        };
        match input.connect(&port, "luma-input", heard, ()) {
            Ok(connection) => {
                open.insert(name, connection);
            }
            Err(error) => log::warn!("[inputs] could not open MIDI {name}: {error}"),
        }
    }
}

/// Keep an output open for each of `inputs` that has one, and none other.
fn scan_outputs(scanner: &MidiOutput, inputs: &[String]) {
    let present: Vec<(String, midir::MidiOutputPort)> = scanner
        .ports()
        .into_iter()
        .filter_map(|port| Some((scanner.port_name(&port).ok()?, port)))
        .filter(|(name, _)| inputs.contains(name))
        .collect();
    let mut open = outputs();
    open.retain(|name, _| present.iter().any(|(known, _)| known == name));
    for (name, port) in present {
        if open.contains_key(&name) {
            continue;
        }
        let Ok(output) = MidiOutput::new(CLIENT) else {
            continue;
        };
        match output.connect(&port, "luma-lights") {
            Ok(connection) => {
                open.insert(name, connection);
            }
            Err(error) => log::warn!("[inputs] could not open MIDI output {name}: {error}"),
        }
    }
}

/// Run this thread's run loop for `time`, so CoreMIDI can tell the process
/// about ports plugged in and pulled out.
#[cfg(target_os = "macos")]
fn wait(time: Duration) {
    use core_foundation_sys::runloop::{
        kCFRunLoopDefaultMode, kCFRunLoopRunFinished, CFRunLoopRunInMode,
    };
    // SAFETY: runs the calling thread's own run loop in the default mode.
    let ran = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, time.as_secs_f64(), 0) };
    // A run loop with nothing on it, before CoreMIDI has a client here,
    // returns at once.
    if ran == kCFRunLoopRunFinished {
        std::thread::sleep(time);
    }
}

/// Elsewhere every scan asks the system afresh.
#[cfg(not(target_os = "macos"))]
fn wait(time: Duration) {
    std::thread::sleep(time);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cc_and_notes_read_0_to_1() {
        let cc = parse(&[0xb2, 7, 127]).unwrap();
        assert_eq!(
            cc.address,
            Address::MidiCc {
                channel: 2,
                number: 7
            }
        );
        assert_eq!(cc.value, 1.0);
        let on = parse(&[0x90, 36, 64]).unwrap();
        assert!(on.value > 0.5);
        let off = parse(&[0x80, 36, 64]).unwrap();
        assert_eq!((off.address, off.value), (on.address, 0.0));
        assert!(parse(&[0xf8]).is_none(), "a clock tick moves nothing");
    }

    /// A port another process plugs in after listening starts is opened, and
    /// dropped when it goes, across a listener closed and opened again (as
    /// signing in does). CoreMIDI tells a process about other processes'
    /// ports only on the run loop of the thread that made its first client:
    /// this fails if that thread sleeps instead of running its run loop, or
    /// ends. A port this process made would show up regardless.
    #[cfg(target_os = "macos")]
    #[test]
    fn ports_plugged_in_later_are_heard() {
        use std::process::{Command, Stdio};
        use std::sync::mpsc;

        let port = format!("Luma test pad {}", std::process::id());
        let listen_for_ports = || {
            let (tx, rx) = mpsc::channel();
            let listener = listen(|_| {}, move |ports| drop(tx.send(ports)));
            (listener, rx)
        };
        let until = |rx: &mpsc::Receiver<Vec<String>>, open: bool| loop {
            let ports = rx
                .recv_timeout(Duration::from_secs(20))
                .expect("the ports are reported");
            if ports.contains(&port) == open {
                break;
            }
        };

        let (first, rx) = listen_for_ports();
        until(&rx, false);
        drop(first);

        let (_listener, rx) = listen_for_ports();
        until(&rx, false);
        let mut pad = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "inputs::midi::tests::a_pad_in_another_process"])
            .args(["--ignored", "--nocapture"])
            .env("LUMA_TEST_MIDI_PORT", &port)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        until(&rx, true);
        drop(pad.stdin.take());
        pad.wait().unwrap();
        until(&rx, false);
    }

    /// The other process for `ports_plugged_in_later_are_heard`: a virtual
    /// port named `LUMA_TEST_MIDI_PORT`, open until stdin closes.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "run by ports_plugged_in_later_are_heard as another process"]
    fn a_pad_in_another_process() {
        use midir::os::unix::VirtualOutput;
        use std::io::Read;

        let name = std::env::var("LUMA_TEST_MIDI_PORT").unwrap();
        let _pad = midir::MidiOutput::new("luma-test-pad")
            .unwrap()
            .create_virtual(&name)
            .unwrap();
        std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
    }
}
