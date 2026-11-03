//! Every MIDI input port, opened as it appears and dropped as it goes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use midir::{MidiInput, MidiInputConnection};

use super::{Address, Event};

const CLIENT: &str = "Luma";
/// How often to look for ports plugged in or pulled out.
const SCAN_EVERY: Duration = Duration::from_secs(1);

/// The scanning thread. Dropping it closes every port.
pub(super) struct Listener {
    stop: Arc<AtomicBool>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
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
    })
}

/// Open every port, now and as they come, handing each message to `sink`
/// and each change of the open ports to `ports`.
pub(super) fn listen(
    sink: impl Fn(Event) + Send + Sync + 'static,
    ports: impl Fn(Vec<String>) + Send + 'static,
) -> Listener {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let sink = Arc::new(sink);
    std::thread::Builder::new()
        .name("luma-midi-inputs".into())
        .spawn(move || {
            let mut open: BTreeMap<String, MidiInputConnection<()>> = BTreeMap::new();
            let mut reported: Option<Vec<String>> = None;
            while !stopped.load(Ordering::Relaxed) {
                if let Ok(scanner) = MidiInput::new(CLIENT) {
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
                        let sink = sink.clone();
                        match input.connect(
                            &port,
                            "luma-input",
                            move |_, data, _| {
                                if let Some(event) = parse(data) {
                                    sink(event);
                                }
                            },
                            (),
                        ) {
                            Ok(connection) => {
                                open.insert(name, connection);
                            }
                            Err(error) => {
                                log::warn!("[inputs] could not open MIDI {name}: {error}")
                            }
                        }
                    }
                }
                let names: Vec<String> = open.keys().cloned().collect();
                if reported.as_ref() != Some(&names) {
                    ports(names.clone());
                    reported = Some(names);
                }
                std::thread::sleep(SCAN_EVERY);
            }
        })
        .expect("spawn the MIDI input thread");
    Listener { stop }
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
}
