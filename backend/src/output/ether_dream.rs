//! Ether Dream DACs, over the network (<https://ether-dream.com/protocol.html>).
//!
//! A DAC announces itself once a second with a UDP broadcast, which is how
//! it is found and what names it (its MAC address). It takes one TCP
//! connection, down which points are written into its buffer; it plays them
//! at the rate it was told and answers every command with its status,
//! which says how full the buffer is.
//!
//! One thread listens for the broadcasts. One watches the routes and keeps
//! a thread per bound projector, which connects to its DAC and keeps its
//! buffer about [`BUFFER_S`] ahead with the projector's newest scan, played
//! round and round. A new frame is taken up where the scan in hand ends, so
//! the mirrors are never asked to jump with the light on. With laser output
//! off, or nothing to draw, the DAC is stopped: it holds no points and its
//! shutter is closed.
//!
//! Everything is little-endian. Not yet run against hardware: the tests
//! play an emulator written from the protocol page.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::laser::{LaserDriver, Shared, WireFrame, WirePoint};
use crate::models::lasers::{LaserDevice, LaserDeviceKind, LaserLink, LaserRoute};

/// Where a DAC broadcasts its status.
const BROADCAST_PORT: u16 = 7654;
/// Where it takes its one connection.
const STREAM_PORT: u16 = 7765;
/// How far ahead of the mirrors the DAC's buffer is kept, seconds: what the
/// network may stall for before the picture breaks, and how late the lasers
/// run behind the lights.
const BUFFER_S: f64 = 0.04;
/// The fewest points worth a packet.
const BATCH: usize = 80;
/// The buffer of a DAC that has not said how large its own is: the first
/// Ether Dream's.
const CAPACITY: usize = 1800;
/// How long a DAC may go unheard before it is no longer listed.
const HEARD: Duration = Duration::from_secs(3);
const TIMEOUT: Duration = Duration::from_secs(1);
/// How often the routes are read, and a quiet link is asked for its status.
const POLL: Duration = Duration::from_millis(100);

const ACK: u8 = b'a';
const IDLE: u8 = 0;
const PREPARED: u8 = 1;
const E_STOP: u8 = 3;

/// A DAC's status, as every answer and broadcast carries it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Status {
    light_engine: u8,
    playback: u8,
    light_engine_flags: u16,
    fullness: u16,
}

impl Status {
    const LEN: usize = 20;

    fn read(bytes: &[u8; Self::LEN]) -> Self {
        let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        Status {
            light_engine: bytes[1],
            playback: bytes[2],
            light_engine_flags: u16_at(4),
            fullness: u16_at(10),
        }
    }

    /// Why the DAC is in emergency stop.
    fn fault(&self) -> String {
        let flags = self.light_engine_flags;
        let why = [
            (0b100, "the projector's emergency stop is pressed"),
            (0b1_0000, "the DAC is too hot"),
            (0b10, "the projector's emergency stop was pressed"),
            (0b1000, "the DAC overheated"),
            (0b10_0000, "the DAC lost its network link"),
        ]
        .into_iter()
        .find(|(bit, _)| flags & bit != 0)
        .map_or("the DAC is in emergency stop", |(_, why)| why);
        why.to_owned()
    }
}

/// A DAC heard on the network.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Heard {
    /// Where it takes its connection.
    address: SocketAddr,
    capacity: usize,
    max_rate: u32,
    at: Instant,
}

/// The DAC a broadcast from `from` announces: its id and what it said.
fn heard(datagram: &[u8], from: SocketAddr) -> Option<(String, Heard)> {
    let bytes: &[u8; 16 + Status::LEN] = datagram.try_into().ok()?;
    let id = bytes[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":");
    Some((
        id,
        Heard {
            address: SocketAddr::new(from.ip(), STREAM_PORT),
            capacity: usize::from(u16::from_le_bytes([bytes[10], bytes[11]])),
            max_rate: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            at: Instant::now(),
        },
    ))
}

type Seen = Arc<Mutex<HashMap<String, Heard>>>;

pub(crate) struct EtherDream {
    seen: Seen,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl EtherDream {
    pub fn start(shared: Arc<Shared>) -> Self {
        let seen = Seen::default();
        let stop = Arc::new(AtomicBool::new(false));
        let spawn = |name: &str, run: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name.into())
                .spawn(run)
                .expect("spawn an Ether Dream thread")
        };
        let threads = vec![
            spawn("ether-dream-listen", {
                let (seen, stop) = (seen.clone(), stop.clone());
                Box::new(move || listen(&seen, &stop))
            }),
            spawn("ether-dream-routes", {
                let (seen, stop) = (seen.clone(), stop.clone());
                Box::new(move || supervise(&shared, &seen, &stop))
            }),
        ];
        Self {
            seen,
            stop,
            threads,
        }
    }
}

impl LaserDriver for EtherDream {
    fn devices(&self) -> Vec<LaserDevice> {
        let mut devices: Vec<LaserDevice> = self
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, heard)| heard.at.elapsed() < HEARD)
            .map(|(id, heard)| LaserDevice {
                kind: LaserDeviceKind::EtherDream,
                id: id.clone(),
                name: format!("Ether Dream {}", &id[id.len() - 5..]),
                address: heard.address.to_string(),
                max_rate: heard.max_rate,
            })
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        devices
    }
}

impl Drop for EtherDream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Hear every DAC on the network until told to stop. Another program
/// holding the port is tried again: a DAC can still be bound by address.
fn listen(seen: &Seen, stop: &AtomicBool) {
    let mut socket = None;
    let mut datagram = [0u8; 64];
    while !stop.load(Ordering::Relaxed) {
        let Some(open) = &socket else {
            socket = UdpSocket::bind(("0.0.0.0", BROADCAST_PORT))
                .and_then(|socket| socket.set_read_timeout(Some(POLL)).map(|()| socket))
                .ok();
            if socket.is_none() {
                std::thread::sleep(TIMEOUT);
            }
            continue;
        };
        if let Ok((length, from)) = open.recv_from(&mut datagram) {
            if let Some((id, heard)) = heard(&datagram[..length], from) {
                seen.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, heard);
            }
        }
    }
}

/// Keep one playing thread per projector bound to an Ether Dream, as the
/// routes say now.
fn supervise(shared: &Arc<Shared>, seen: &Seen, stop: &AtomicBool) {
    let mut playing: HashMap<LaserRoute, (Arc<AtomicBool>, JoinHandle<()>)> = HashMap::new();
    while !stop.load(Ordering::Relaxed) && shared.running() {
        let routes = shared.routes();
        let bound = |route: &LaserRoute| {
            route.device_kind == LaserDeviceKind::EtherDream && routes.contains(route)
        };
        let gone: Vec<LaserRoute> = playing.keys().filter(|r| !bound(r)).cloned().collect();
        for route in gone {
            if let Some((halt, thread)) = playing.remove(&route) {
                halt.store(true, Ordering::Relaxed);
                let _ = thread.join();
            }
        }
        for route in routes.iter().filter(|route| bound(route)) {
            if playing.contains_key(route) {
                continue;
            }
            let halt = Arc::new(AtomicBool::new(false));
            let thread = std::thread::Builder::new()
                .name("ether-dream-play".into())
                .spawn({
                    let (shared, seen) = (shared.clone(), seen.clone());
                    let (route, halt) = (route.clone(), halt.clone());
                    move || play(&shared, &seen, &route, &halt)
                })
                .expect("spawn an Ether Dream thread");
            playing.insert(route.clone(), (halt, thread));
        }
        std::thread::sleep(POLL);
    }
    for (_, (halt, thread)) in playing {
        halt.store(true, Ordering::Relaxed);
        let _ = thread.join();
    }
}

/// Play `route`'s projector through its DAC until the route goes: connect,
/// play until the link breaks, and connect again.
fn play(shared: &Shared, seen: &Seen, route: &LaserRoute, halt: &AtomicBool) {
    let wanted = || !halt.load(Ordering::Relaxed) && shared.running();
    while wanted() {
        shared.report(&route.fixture_id, Some(LaserLink::Searching));
        let heard = seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&route.device_id)
            .filter(|heard| heard.at.elapsed() < HEARD)
            .cloned();
        // The DAC as it was last heard, or where the route says to look.
        let address = heard.as_ref().map(|heard| heard.address).or_else(|| {
            route
                .address
                .as_deref()
                .and_then(|address| address.to_socket_addrs().ok()?.next())
        });
        let capacity = heard.map_or(CAPACITY, |heard| heard.capacity);
        let played = address
            .ok_or_else(|| io::Error::other("not found"))
            .and_then(Link::connect)
            .and_then(|mut link| {
                let played = link.play(shared, &route.fixture_id, capacity, &wanted);
                // Leave it dark, however the playing ended.
                let _ = link.stop();
                played
            });
        if let Err(error) = played {
            log::debug!("[laser] {}: {error}", route.fixture_id);
            std::thread::sleep(POLL);
        }
    }
    shared.report(&route.fixture_id, None);
}

/// Where in which frame the stream has got to.
#[derive(Clone, Default)]
struct Cursor {
    frame: Option<Arc<WireFrame>>,
    index: usize,
    /// Where the mirrors were last sent.
    at: [i16; 2],
}

impl Cursor {
    /// The next `count` points or so of the stream. The scan in hand is
    /// finished before `latest` is taken up, and the mirrors are walked to
    /// its start in the dark.
    fn take(&mut self, latest: &Arc<WireFrame>, count: usize) -> Vec<WirePoint> {
        let mut points = Vec::with_capacity(count);
        while points.len() < count {
            let current = match &self.frame {
                Some(frame) if self.index != 0 || Arc::ptr_eq(frame, latest) => frame.clone(),
                _ => {
                    points.extend(walk(self.at, latest.points[0], latest.blank_step));
                    self.frame = Some(latest.clone());
                    self.index = 0;
                    latest.clone()
                }
            };
            let point = current.points[self.index];
            self.index = (self.index + 1) % current.points.len();
            self.at = [point.x, point.y];
            points.push(point);
        }
        points
    }
}

/// Dark points from `from` to where `to` is, each at most `step` from the
/// last on either axis: the mirrors arrive, dark, before `to` is sent.
fn walk(from: [i16; 2], to: WirePoint, step: u16) -> impl Iterator<Item = WirePoint> {
    let delta = [to.x, to.y].map(i32::from);
    let from = from.map(i32::from);
    let far = (delta[0] - from[0]).abs().max((delta[1] - from[1]).abs());
    let step = i32::from(step.max(1));
    let steps = (far + step - 1) / step;
    (1..=steps).map(move |k| {
        let at = |axis: usize| (from[axis] + (delta[axis] - from[axis]) * k / steps) as i16;
        WirePoint {
            x: at(0),
            y: at(1),
            rgb: [0; 3],
        }
    })
}

/// One connection to a DAC, and the status it last gave.
struct Link {
    stream: TcpStream,
    status: Status,
}

impl Link {
    fn connect(address: SocketAddr) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        let mut link = Link {
            stream,
            status: Status::default(),
        };
        // A DAC greets a connection as it answers a ping.
        link.answer(b'?')?;
        Ok(link)
    }

    /// Read the answer to `command`: whether it was taken, and the status.
    fn answer(&mut self, command: u8) -> io::Result<u8> {
        let mut answer = [0u8; 2 + Status::LEN];
        self.stream.read_exact(&mut answer)?;
        if answer[1] != command {
            return Err(io::Error::other(format!(
                "the DAC answered {:?} to {:?}",
                answer[1] as char, command as char
            )));
        }
        let status: &[u8; Status::LEN] = answer[2..].try_into().expect("a status");
        self.status = Status::read(status);
        Ok(answer[0])
    }

    /// Send a command; whether the DAC took it.
    fn command(&mut self, bytes: &[u8]) -> io::Result<bool> {
        self.stream.write_all(bytes)?;
        Ok(self.answer(bytes[0])? == ACK)
    }

    fn stop(&mut self) -> io::Result<()> {
        if self.status.playback != IDLE {
            self.command(b"s")?;
        }
        Ok(())
    }

    /// Write `points` into the DAC's buffer; whether it had room.
    fn write(&mut self, points: &[WirePoint]) -> io::Result<bool> {
        let mut packet = Vec::with_capacity(3 + 18 * points.len());
        packet.push(b'd');
        packet.extend((points.len() as u16).to_le_bytes());
        for point in points {
            // The intensity line follows the brightest diode: a projector
            // that blanks on it is dark exactly when its diodes are.
            let intensity = point.rgb.into_iter().max().unwrap_or(0);
            packet.extend(0u16.to_le_bytes());
            packet.extend(point.x.to_le_bytes());
            packet.extend(point.y.to_le_bytes());
            for level in point.rgb.into_iter().chain([intensity, 0, 0]) {
                packet.extend(level.to_le_bytes());
            }
        }
        self.command(&packet)
    }

    /// Keep the DAC playing `projector`'s frames while `wanted`.
    fn play(
        &mut self,
        shared: &Shared,
        projector: &str,
        capacity: usize,
        wanted: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        let mut cursor = Cursor::default();
        // The rate the DAC was last told to play at.
        let mut rate = None;
        while wanted() {
            let frame = shared.frame(projector).filter(|f| !f.points.is_empty());
            let Some(frame) = frame else {
                self.stop()?;
                (cursor.frame, rate) = (None, None);
                shared.report(projector, Some(LaserLink::Idle));
                std::thread::sleep(POLL);
                self.command(b"?")?;
                continue;
            };
            if self.status.light_engine == E_STOP && !self.command(b"c")? {
                shared.report(projector, Some(LaserLink::Fault(self.status.fault())));
                std::thread::sleep(POLL);
                continue;
            }
            // A scanner's rate changes only with its settings: start over.
            if rate.is_some_and(|rate| rate != frame.rate) {
                self.stop()?;
                (cursor.frame, rate) = (None, None);
            }
            if self.status.playback == IDLE && !self.command(b"p")? {
                let fault = "the DAC will not take a stream".to_owned();
                shared.report(projector, Some(LaserLink::Fault(fault)));
                std::thread::sleep(POLL);
                self.command(b"?")?;
                continue;
            }
            let ahead = ((f64::from(frame.rate) * BUFFER_S) as usize).min(capacity * 3 / 4);
            let room = ahead.saturating_sub(usize::from(self.status.fullness));
            if room < BATCH {
                // Wait for a packet's worth to play, then ask how full.
                std::thread::sleep(Duration::from_secs_f64(
                    BATCH as f64 / f64::from(frame.rate),
                ));
                self.command(b"?")?;
                continue;
            }
            let before = cursor.clone();
            if !self.write(&cursor.take(&frame, room))? {
                // Fuller than it said: these points are still to send.
                cursor = before;
                continue;
            }
            if self.status.playback == PREPARED {
                let mut begin = vec![b'b', 0, 0];
                begin.extend(frame.rate.to_le_bytes());
                if !self.command(&begin)? {
                    continue;
                }
                rate = Some(frame.rate);
            }
            let scans = f64::from(frame.rate) / frame.points.len() as f64;
            shared.report(projector, Some(LaserLink::Playing(scans)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::net::TcpListener;

    /// An Ether Dream as the protocol page describes it: it takes one
    /// connection, keeps a buffer, plays it at the rate it was told and
    /// remembers everything it was sent.
    struct Dac {
        address: SocketAddr,
        state: Arc<Mutex<State>>,
    }

    #[derive(Default)]
    struct State {
        commands: Vec<u8>,
        /// Every point written to it, in order.
        points: Vec<WirePoint>,
        buffer: VecDeque<WirePoint>,
        playback: u8,
        light_engine: u8,
        rate: u32,
        /// When the buffer was last played from.
        played: Option<Instant>,
    }

    impl State {
        /// Play what the time since the last look played.
        fn advance(&mut self) {
            let now = Instant::now();
            if self.playback == 2 {
                let since = now - self.played.unwrap_or(now);
                let due = (since.as_secs_f64() * f64::from(self.rate)) as usize;
                if due >= self.buffer.len() && due > 0 {
                    // Underflow ends the stream.
                    self.buffer.clear();
                    self.playback = IDLE;
                } else {
                    self.buffer.drain(..due);
                }
                if due == 0 {
                    return;
                }
            }
            self.played = Some(now);
        }

        fn answer(&mut self, ok: bool, command: u8) -> [u8; 22] {
            let mut answer = [0u8; 22];
            answer[0] = if ok { ACK } else { b'I' };
            answer[1] = command;
            answer[3] = self.light_engine;
            answer[4] = self.playback;
            answer[12..14].copy_from_slice(&(self.buffer.len() as u16).to_le_bytes());
            let rate = if self.playback == IDLE { 0 } else { self.rate };
            answer[14..18].copy_from_slice(&rate.to_le_bytes());
            answer
        }
    }

    impl Dac {
        fn start(light_engine: u8) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let state = Arc::new(Mutex::new(State {
                light_engine,
                ..State::default()
            }));
            std::thread::spawn({
                let state = state.clone();
                move || {
                    for stream in listener.incoming().flatten() {
                        let _ = serve(stream, &state);
                    }
                }
            });
            Dac { address, state }
        }

        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            let mut state = self.state.lock().unwrap();
            state.advance();
            state
        }

        fn route(&self) -> LaserRoute {
            LaserRoute {
                fixture_id: "p".into(),
                device_kind: LaserDeviceKind::EtherDream,
                device_id: "emulated".into(),
                device_name: None,
                address: Some(self.address.to_string()),
            }
        }
    }

    fn serve(mut stream: TcpStream, state: &Mutex<State>) -> io::Result<()> {
        let greeting = state.lock().unwrap().answer(true, b'?');
        stream.write_all(&greeting)?;
        loop {
            let mut command = [0u8; 1];
            stream.read_exact(&mut command)?;
            let command = command[0];
            let mut body = vec![0u8; if command == b'b' { 6 } else { 0 }];
            if command == b'd' {
                let mut count = [0u8; 2];
                stream.read_exact(&mut count)?;
                body = vec![0u8; 18 * usize::from(u16::from_le_bytes(count))];
            }
            stream.read_exact(&mut body)?;
            let mut state = state.lock().unwrap();
            state.advance();
            state.commands.push(command);
            let ok = match command {
                b'?' => true,
                b'c' if state.light_engine == E_STOP => {
                    state.light_engine = 0;
                    true
                }
                b'p' if state.light_engine == 0 && state.playback == IDLE => {
                    state.buffer.clear();
                    state.playback = PREPARED;
                    true
                }
                b'd' if state.playback != IDLE => {
                    let points: Vec<WirePoint> = body
                        .chunks_exact(18)
                        .map(|p| {
                            let word = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
                            WirePoint {
                                x: word(2) as i16,
                                y: word(4) as i16,
                                rgb: [word(6), word(8), word(10)],
                            }
                        })
                        .collect();
                    state.points.extend(&points);
                    state.buffer.extend(points);
                    true
                }
                b'b' if state.playback == PREPARED && !state.buffer.is_empty() => {
                    state.rate = u32::from_le_bytes([body[2], body[3], body[4], body[5]]);
                    state.playback = 2;
                    state.played = Some(Instant::now());
                    true
                }
                b's' if state.playback != IDLE => {
                    state.buffer.clear();
                    state.playback = IDLE;
                    true
                }
                _ => false,
            };
            let answer = state.answer(ok, command);
            drop(state);
            stream.write_all(&answer)?;
        }
    }

    /// Wait until `met`, as a DAC's thread makes it so.
    fn until(what: &str, met: impl Fn() -> bool) {
        let give_up = Instant::now() + Duration::from_secs(20);
        while !met() {
            assert!(Instant::now() < give_up, "never: {what}");
            std::thread::yield_now();
        }
    }

    /// A scan of `lit` points at `x`, then as many dark ones.
    fn scan(x: i16, lit: usize) -> Arc<WireFrame> {
        let point = |rgb| WirePoint { x, y: 0, rgb };
        Arc::new(WireFrame {
            rate: 30_000,
            points: (0..lit)
                .map(|_| point([u16::MAX, 0, 0]))
                .chain((0..lit).map(|_| point([0; 3])))
                .collect(),
            blank_step: 2_000,
        })
    }

    fn rig(dac: &Dac) -> (Arc<Shared>, EtherDream) {
        let shared = Arc::new(Shared::new(vec![dac.route()]));
        let driver = EtherDream::start(shared.clone());
        (shared, driver)
    }

    fn show(shared: &Shared, frame: Arc<WireFrame>) {
        shared.show("p", Some(frame));
    }

    fn sent(dac: &Dac, command: u8) -> usize {
        dac.state()
            .commands
            .iter()
            .filter(|c| **c == command)
            .count()
    }

    /// Output is off until it is turned on: the DAC is connected and asked
    /// how it is, and sent no point. Turned on, it plays at the scanner's
    /// rate; turned off, it is stopped.
    #[test]
    fn a_dac_plays_only_while_output_is_on() {
        let dac = Dac::start(0);
        let (shared, _driver) = rig(&dac);
        show(&shared, scan(1_000, 50));
        until("the idle link is polled", || sent(&dac, b'?') >= 2);
        assert_eq!(sent(&dac, b'd') + sent(&dac, b'p') + sent(&dac, b'b'), 0);
        assert_eq!(shared.links()["p"], LaserLink::Idle);

        shared.set_on(true);
        until("the DAC plays", || dac.state().playback == 2);
        assert_eq!(dac.state().rate, 30_000);
        until("the link says so", || {
            shared.links()["p"] == LaserLink::Playing(300.)
        });
        assert!(dac.state().points.iter().any(|p| p.rgb[0] == u16::MAX));

        shared.set_on(false);
        until("the DAC is stopped", || {
            let state = dac.state();
            state.playback == IDLE && state.commands.last() != Some(&b'd')
        });
        until("the link says so", || {
            shared.links()["p"] == LaserLink::Idle
        });
    }

    /// The stream keeps coming: the DAC plays more than its buffer holds
    /// without running dry.
    #[test]
    fn the_buffer_is_kept_fed() {
        let dac = Dac::start(0);
        let (shared, _driver) = rig(&dac);
        shared.set_on(true);
        show(&shared, scan(1_000, 50));
        until("three buffers' worth is sent", || {
            dac.state().points.len() > 3 * CAPACITY
        });
        assert_eq!(sent(&dac, b'p'), 1, "the stream never ran dry");
    }

    /// A DAC found in emergency stop is cleared before it is asked to play.
    #[test]
    fn an_emergency_stop_is_cleared_first() {
        let dac = Dac::start(E_STOP);
        let (shared, _driver) = rig(&dac);
        shared.set_on(true);
        show(&shared, scan(1_000, 50));
        until("the DAC plays", || dac.state().playback == 2);
        let commands = dac.state().commands.clone();
        let first = |command| commands.iter().position(|c| *c == command).unwrap();
        assert!(first(b'c') < first(b'p'));
    }

    /// A new frame is taken up at the end of the scan in hand, and the
    /// mirrors are walked to it in the dark: no lit point is ever a long
    /// move from the point before it.
    #[test]
    fn a_new_frame_is_reached_in_the_dark() {
        let dac = Dac::start(0);
        let (shared, _driver) = rig(&dac);
        shared.set_on(true);
        show(&shared, scan(-20_000, 50));
        until("the first frame plays", || !dac.state().points.is_empty());
        show(&shared, scan(20_000, 50));
        until("the second frame plays", || {
            dac.state()
                .points
                .iter()
                .any(|p| p.x == 20_000 && p.rgb[0] > 0)
        });
        let points = dac.state().points.clone();
        for pair in points.windows(2) {
            let moved = (i32::from(pair[1].x) - i32::from(pair[0].x)).abs();
            assert!(moved <= 2_000, "a {moved} move: {pair:?}");
            assert!(
                moved == 0 || pair[1].rgb == [0; 3],
                "lit on the move: {pair:?}"
            );
        }
        // The first frame's scans are whole: none is cut short.
        let first = points.iter().filter(|p| p.x == -20_000 && p.rgb[0] > 0);
        assert_eq!(first.count() % 50, 0);
    }

    /// Unbound, a projector's DAC is let go and no longer reported.
    #[test]
    fn an_unbound_projector_is_let_go() {
        let dac = Dac::start(0);
        let (shared, _driver) = rig(&dac);
        until("the link is up", || shared.links().contains_key("p"));
        shared.set_routes(Vec::new());
        until("the link is gone", || shared.links().is_empty());
    }

    #[test]
    fn a_broadcast_names_its_dac() {
        let mut datagram = [0u8; 36];
        datagram[..6].copy_from_slice(&[0x00, 0x04, 0xa3, 0x87, 0x28, 0xcd]);
        datagram[10..12].copy_from_slice(&1799u16.to_le_bytes());
        datagram[12..16].copy_from_slice(&100_000u32.to_le_bytes());
        let (id, dac) = heard(&datagram, "10.0.0.7:7654".parse().unwrap()).unwrap();
        assert_eq!(id, "00:04:a3:87:28:cd");
        assert_eq!(dac.address, "10.0.0.7:7765".parse().unwrap());
        assert_eq!((dac.capacity, dac.max_rate), (1799, 100_000));
        assert!(heard(&datagram[..20], "10.0.0.7:7654".parse().unwrap()).is_none());
    }
}
