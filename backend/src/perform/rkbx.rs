//! Rekordbox decks through rkbx_link.
//!
//! rkbx_link (<https://github.com/grufkork/rkbx_link>, GPLv3) reads Rekordbox's
//! memory and sends what it finds as OSC over UDP. Luma only listens on the
//! network and shares no code with it. Its messages, per deck `n` (0-based):
//!
//! - `/n/time` (float seconds), sent only when it changes, and only when
//!   `osc.msg.n/time` is on in rkbx_link's config, which it is not by default
//! - `/n/bpm/current`, `/n/bpm/original` (float), sent when they change
//! - `/n/track/{title,artist,album}` (string), sent when the track changes, so
//!   a deck loaded before Luma started listening stays unknown
//!
//! `/master/...` repeats the master deck's values and is ignored. Nothing says
//! whether a deck is playing; the follower infers it from `/time` moving.

use std::net::SocketAddr;
use std::time::Instant;

use rosc::{OscPacket, OscType};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use super::{LoadedTrack, Reading};

/// rkbx_link's default `osc.destination`.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:4460";

/// The decks rkbx_link can track (`keeper.decks` is 1 to 4).
const DECKS: usize = 4;

#[derive(Clone, Debug, PartialEq)]
enum Field {
    Time(f64),
    BpmCurrent(f64),
    BpmOriginal(f64),
    Title(String),
    Artist(String),
}

/// One per-deck message, with its deck index (0-based, as rkbx_link sends it).
#[derive(Clone, Debug, PartialEq)]
struct Message {
    deck: usize,
    field: Field,
}

fn parse(addr: &str, args: &[OscType]) -> Option<Message> {
    let mut parts = addr.strip_prefix('/')?.split('/');
    let deck: usize = parts.next()?.parse().ok()?;
    if deck >= DECKS {
        return None;
    }
    let path: Vec<&str> = parts.collect();
    let float = || match args.first()? {
        OscType::Float(value) => Some(f64::from(*value)),
        OscType::Double(value) => Some(*value),
        OscType::Int(value) => Some(f64::from(*value)),
        _ => None,
    };
    let string = || match args.first()? {
        OscType::String(value) => Some(value.clone()),
        _ => None,
    };
    let field = match path.as_slice() {
        ["time"] => Field::Time(float()?),
        ["bpm", "current"] => Field::BpmCurrent(float()?),
        ["bpm", "original"] => Field::BpmOriginal(float()?),
        ["track", "title"] => Field::Title(string()?),
        ["track", "artist"] => Field::Artist(string()?),
        _ => return None,
    };
    Some(Message { deck, field })
}

fn messages(packet: OscPacket, out: &mut Vec<Message>) {
    match packet {
        OscPacket::Message(message) => out.extend(parse(&message.addr, &message.args)),
        OscPacket::Bundle(bundle) => {
            for packet in bundle.content {
                messages(packet, out);
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Deck {
    title: String,
    artist: String,
    bpm: f64,
    original_bpm: f64,
    time: Option<f64>,
}

/// What rkbx_link has said so far, per deck. rkbx_link sends each field on
/// its own, so a reading is the whole deck as of the latest field.
#[derive(Debug, Default)]
struct Decks {
    decks: [Deck; DECKS],
}

impl Decks {
    fn apply(&mut self, message: Message) -> Reading {
        let deck = &mut self.decks[message.deck];
        let mut moved = false;
        match message.field {
            Field::Time(time) => {
                deck.time = Some(time);
                moved = true;
            }
            Field::BpmCurrent(bpm) => deck.bpm = bpm,
            Field::BpmOriginal(bpm) => deck.original_bpm = bpm,
            Field::Title(title) => deck.title = title,
            Field::Artist(artist) => deck.artist = artist,
        }
        let rate = if deck.original_bpm > 0.0 && deck.bpm > 0.0 {
            deck.bpm / deck.original_bpm
        } else {
            1.0
        };
        let loaded = !(deck.title.is_empty() && deck.artist.is_empty());
        Reading {
            // Luma numbers decks from 1, as players and mixers do.
            deck: message.deck as u8 + 1,
            track: loaded.then(|| LoadedTrack {
                title: deck.title.clone(),
                artist: deck.artist.clone(),
                network_path: None,
                duration: None,
                bpm: (deck.original_bpm > 0.0).then_some(deck.original_bpm),
            }),
            // Only a `/time` message is a new observation of the playhead.
            position: if moved { deck.time } else { None },
            playing: None,
            bpm: deck.bpm,
            rate,
            volume: 1.0,
        }
    }
}

/// A bound listener. Dropping it stops listening.
pub(crate) struct Listener {
    task: JoinHandle<()>,
    pub(crate) address: SocketAddr,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Bind `address` and hand every per-deck reading, with the instant its
/// packet arrived, to `sink`. `sink` also hears each packet's arrival, as
/// `None`, so a caller can tell a quiet rkbx_link from an absent one.
pub(crate) async fn listen(
    address: &str,
    mut sink: impl FnMut(Option<Reading>, Instant) + Send + 'static,
) -> Result<Listener, String> {
    let socket = UdpSocket::bind(address)
        .await
        .map_err(|error| format!("Could not listen on {address}: {error}"))?;
    let address = socket
        .local_addr()
        .map_err(|error| format!("Could not read the listening address: {error}"))?;
    let task = tokio::spawn(async move {
        let mut decks = Decks::default();
        let mut buffer = vec![0u8; rosc::decoder::MTU];
        let mut parsed = Vec::new();
        loop {
            let Ok((len, _from)) = socket.recv_from(&mut buffer).await else {
                continue;
            };
            let at = Instant::now();
            let Ok((_, packet)) = rosc::decoder::decode_udp(&buffer[..len]) else {
                continue;
            };
            parsed.clear();
            messages(packet, &mut parsed);
            sink(None, at);
            for message in parsed.drain(..) {
                sink(Some(decks.apply(message)), at);
            }
        }
    });
    Ok(Listener { task, address })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::OscMessage;
    use std::sync::{Arc, Mutex};

    fn float(addr: &str, value: f32) -> OscPacket {
        OscPacket::Message(OscMessage {
            addr: addr.into(),
            args: vec![OscType::Float(value)],
        })
    }

    fn string(addr: &str, value: &str) -> OscPacket {
        OscPacket::Message(OscMessage {
            addr: addr.into(),
            args: vec![OscType::String(value.into())],
        })
    }

    fn feed(decks: &mut Decks, packet: OscPacket) -> Vec<Reading> {
        let mut parsed = Vec::new();
        messages(packet, &mut parsed);
        parsed.into_iter().map(|m| decks.apply(m)).collect()
    }

    #[test]
    fn decks_are_numbered_from_one_and_master_is_ignored() {
        let mut decks = Decks::default();
        assert!(feed(&mut decks, float("/master/time", 3.0)).is_empty());
        assert!(feed(&mut decks, float("/9/time", 3.0)).is_empty());
        let reading = feed(&mut decks, float("/0/time", 3.0)).pop().unwrap();
        assert_eq!(reading.deck, 1);
        assert_eq!(reading.position, Some(3.0));
    }

    #[test]
    fn a_deck_accumulates_its_track_and_tempo() {
        let mut decks = Decks::default();
        feed(&mut decks, string("/1/track/title", "Strobe"));
        feed(&mut decks, string("/1/track/artist", "deadmau5"));
        feed(&mut decks, float("/1/bpm/original", 128.0));
        let reading = feed(&mut decks, float("/1/bpm/current", 131.84))
            .pop()
            .unwrap();
        let track = reading.track.unwrap();
        assert_eq!(
            (track.title.as_str(), track.artist.as_str()),
            ("Strobe", "deadmau5")
        );
        assert_eq!(track.bpm, Some(128.0));
        assert!(
            (reading.rate - 1.03).abs() < 1e-3,
            "rate is current over original"
        );
        assert_eq!(
            reading.position, None,
            "a tempo message says nothing new about the playhead"
        );
    }

    #[test]
    fn bundles_are_unpacked() {
        let mut decks = Decks::default();
        let bundle = OscPacket::Bundle(rosc::OscBundle {
            timetag: rosc::OscTime {
                seconds: 0,
                fractional: 1,
            },
            content: vec![float("/0/time", 1.0), float("/1/time", 2.0)],
        });
        let decks_seen: Vec<u8> = feed(&mut decks, bundle).iter().map(|r| r.deck).collect();
        assert_eq!(decks_seen, vec![1, 2]);
    }

    #[tokio::test]
    async fn readings_arrive_over_udp() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = heard.clone();
        let listener = listen("127.0.0.1:0", move |reading, _| {
            if let Some(reading) = reading {
                sink.lock().unwrap().push(reading);
            }
        })
        .await
        .unwrap();

        let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let packet = rosc::encoder::encode(&float("/2/time", 42.5)).unwrap();
        sender.send_to(&packet, listener.address).unwrap();

        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(reading) = heard.lock().unwrap().first() {
                assert_eq!((reading.deck, reading.position), (3, Some(42.5)));
                break;
            }
            assert!(Instant::now() < deadline, "no reading arrived");
            tokio::task::yield_now().await;
        }
    }
}
