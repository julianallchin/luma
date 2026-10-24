//! USB DMX dongles. The SoundSwitch Micro and Control One are the first.
//!
//! One thread watches the bus for supported devices; each one found gets a
//! thread of its own, because a bulk write blocks and a dongle that stalls
//! must stall only itself. A device thread sends the start-up packets, then a
//! frame every 25 ms until the device goes away or output turns off, and
//! blacks the device out as it stops.
//!
//! The SoundSwitch protocol is from the Apache-2.0 QLC+ integration at
//! <https://github.com/jloops412/qlcplus-soundswitch-integration>
//! (`qlcplus/plugins/soundswitch/soundswitchprotocol.h`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nusb::transfer::{Buffer, Bulk, Out};
use nusb::MaybeFuture as _;

use super::{OutputDriver, Shared};
use crate::models::patch::{DeviceKind, OutputDevice, OutputPort};

const SCAN_INTERVAL: Duration = Duration::from_secs(1);
const FRAME_INTERVAL: Duration = Duration::from_millis(25);
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

/// A dongle this driver knows how to speak to.
struct Model {
    vendor_id: u16,
    product_id: u16,
    name: &'static str,
    outputs: u8,
    /// Written once on open, before the first frame.
    start: &'static [[u8; 12]],
    /// How long the device needs after the start packets.
    settle: Duration,
}

const SOUNDSWITCH_VENDOR: u16 = 0x15E4;
const SOUNDSWITCH_ENDPOINT: u8 = 0x01;

const MODELS: &[Model] = &[
    Model {
        vendor_id: SOUNDSWITCH_VENDOR,
        product_id: 0x0053,
        name: "SoundSwitch Micro DMX",
        outputs: 1,
        start: &[
            *b"sTRt\x02\x00\x04\x00\x00\x00\x01\x00",
            *b"sTRt\x02\x00\x04\x00\x01\x00\xff\xff",
        ],
        settle: Duration::from_millis(200),
    },
    // The same interface under a newer product id (bcdDevice 2.00), as sold
    // in 2026: one vendor interface, bulk OUT 0x01, 64-byte packets.
    Model {
        vendor_id: SOUNDSWITCH_VENDOR,
        product_id: 0x005A,
        name: "SoundSwitch Micro DMX",
        outputs: 1,
        start: &[
            *b"sTRt\x02\x00\x04\x00\x00\x00\x01\x00",
            *b"sTRt\x02\x00\x04\x00\x01\x00\xff\xff",
        ],
        settle: Duration::from_millis(200),
    },
    Model {
        vendor_id: SOUNDSWITCH_VENDOR,
        product_id: 0x0054,
        name: "SoundSwitch Control One",
        outputs: 2,
        start: &[
            *b"sTRt\x02\x00\x04\x00\x00\x00\x01\x00",
            *b"sTRt\x02\x00\x04\x00\x01\x00\xff\xff",
            *b"sTRt\x02\x00\x04\x00\x00\x00\x01\x00",
            *b"sTRt\x02\x00\x04\x00\x01\x00\x01\x00",
        ],
        settle: Duration::ZERO,
    },
];

/// One SoundSwitch DMX frame: a ten-byte header naming the output, then the
/// 512 slots.
fn soundswitch_frame(port: u8, data: &[u8; 512]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(522);
    packet.extend_from_slice(b"sTRt\x01\x00\x02\x02");
    packet.push(port);
    packet.push(0x00);
    packet.extend_from_slice(data);
    packet
}

/// A device on the bus and the thread driving it.
struct Attached {
    device: OutputDevice,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

type Devices = Arc<Mutex<HashMap<String, Attached>>>;

pub(super) struct UsbDriver {
    devices: Devices,
    thread: Option<JoinHandle<()>>,
}

impl UsbDriver {
    pub fn start(shared: Arc<Shared>) -> Self {
        let devices: Devices = Arc::default();
        let thread = std::thread::Builder::new()
            .name("luma-usb-dmx".into())
            .spawn({
                let devices = devices.clone();
                move || scan(&shared, &devices)
            })
            .map_err(|e| log::error!("[USB DMX] could not start its thread: {e}"))
            .ok();
        Self { devices, thread }
    }
}

impl OutputDriver for UsbDriver {
    fn devices(&self) -> Vec<OutputDevice> {
        let devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<OutputDevice> = devices.values().map(|a| a.device.clone()).collect();
        list.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        list
    }
}

impl Drop for UsbDriver {
    fn drop(&mut self) {
        // The scan thread stops every device thread on its way out.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Watch the bus until shutdown: start a thread for each supported device
/// that appears, and stop all of them while output is off.
fn scan(shared: &Arc<Shared>, devices: &Devices) {
    let mut last_scan: Option<Instant> = None;
    while shared.running() {
        if !shared.enabled.load(Ordering::Relaxed) {
            detach_all(devices);
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if last_scan.is_some_and(|t| t.elapsed() < SCAN_INTERVAL) {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        last_scan = Some(Instant::now());

        // A device thread that has ended lost its device; forget it so the
        // next scan can pick it up again if it is still on the bus.
        devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, a| {
                a.thread.as_ref().is_some_and(|t| !t.is_finished()) || a.device.problem.is_some()
            });

        let found = match nusb::list_devices().wait() {
            Ok(found) => found,
            Err(e) => {
                log::warn!("[USB DMX] could not list USB devices: {e}");
                continue;
            }
        };
        let mut present = Vec::new();
        for info in found {
            let Some(model) = MODELS
                .iter()
                .find(|m| m.vendor_id == info.vendor_id() && m.product_id == info.product_id())
            else {
                continue;
            };
            let id = device_id(&info);
            present.push(id.clone());
            let mut table = devices.lock().unwrap_or_else(|e| e.into_inner());
            if table.contains_key(&id) {
                continue;
            }
            let device = OutputDevice {
                kind: DeviceKind::Usb,
                id: id.clone(),
                name: model.name.into(),
                address: format!("USB {}", bus_path(&info)),
                ports: (0..model.outputs)
                    .map(|i| OutputPort {
                        port: i64::from(i),
                        label: if model.outputs == 1 {
                            "DMX".into()
                        } else {
                            format!("DMX {}", char::from(b'A' + i))
                        },
                    })
                    .collect(),
                any_port: false,
                problem: None,
            };
            let stop = Arc::new(AtomicBool::new(false));
            let thread = std::thread::Builder::new()
                .name(format!("luma-usb-dmx {id}"))
                .spawn({
                    let shared = shared.clone();
                    let stop = stop.clone();
                    let devices = devices.clone();
                    let info = info.clone();
                    let id = id.clone();
                    move || {
                        if let Err(problem) = drive(&shared, &stop, model, &info, &id) {
                            log::warn!("[USB DMX] {id}: {problem}");
                            if let Some(a) = devices
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get_mut(&id)
                            {
                                a.device.problem = Some(problem);
                            }
                        }
                    }
                })
                .map_err(|e| log::error!("[USB DMX] could not start a device thread: {e}"))
                .ok();
            table.insert(
                id,
                Attached {
                    device,
                    stop,
                    thread,
                },
            );
        }
        // A device that failed to open stays listed with its problem while it
        // is plugged in, and is retried once it is unplugged and back.
        devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| present.contains(id));
    }
    detach_all(devices);
}

fn detach_all(devices: &Devices) {
    let attached: Vec<Attached> = devices
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain()
        .map(|(_, a)| a)
        .collect();
    for mut a in attached {
        a.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = a.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What a route names a dongle by: its serial number when it reports one,
/// else the bus and ports it is plugged into, which survives a restart but not
/// a move to another socket.
fn device_id(info: &nusb::DeviceInfo) -> String {
    let at = info
        .serial_number()
        .filter(|s| !s.trim().is_empty())
        .map_or_else(|| bus_path(info), |s| s.trim().to_string());
    format!("{:04x}:{:04x}:{at}", info.vendor_id(), info.product_id())
}

/// `bus-port.port…`, the way Linux names a USB device's place.
fn bus_path(info: &nusb::DeviceInfo) -> String {
    let ports: Vec<String> = info.port_chain().iter().map(u8::to_string).collect();
    format!("{}-{}", info.bus_id(), ports.join("."))
}

/// Open one dongle and send it frames until `stop`, shutdown, or the device
/// goes away. An error is a reason the device cannot be driven at all — no
/// permission, interface held by another program — and is shown on it. A
/// device unplugged mid-stream is not an error: the scan forgets it.
fn drive(
    shared: &Shared,
    stop: &AtomicBool,
    model: &Model,
    info: &nusb::DeviceInfo,
    id: &str,
) -> Result<(), String> {
    let device = info.open().wait().map_err(|e| open_problem(&e))?;
    let interface = device.detach_and_claim_interface(0).wait().map_err(|e| {
        format!("could not claim it: {e}. Is SoundSwitch or another program using it?")
    })?;
    let mut endpoint = interface
        .endpoint::<Bulk, Out>(SOUNDSWITCH_ENDPOINT)
        .map_err(|e| format!("it has no DMX endpoint: {e}"))?;
    let mut write = |packet: Vec<u8>| -> bool {
        let completion = endpoint.transfer_blocking(Buffer::from(packet), WRITE_TIMEOUT);
        match completion.status {
            Ok(()) => true,
            Err(e) => {
                log::warn!("[USB DMX] {id}: write failed ({e}); letting it go");
                false
            }
        }
    };
    for packet in model.start {
        if !write(packet.to_vec()) {
            return Err("it refused the start-up packets".into());
        }
    }
    std::thread::sleep(model.settle);
    log::info!("[USB DMX] driving {} ({id})", model.name);

    let blank = [0u8; 512];
    while shared.running() && !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let (_, frame) = shared.frames.latest();
        let routes = shared.routes();
        for port in 0..model.outputs {
            // A dongle needs a steady stream, so an unrouted output gets
            // zeros rather than silence. Two universes on one output: the
            // first route wins.
            let data = routes
                .iter()
                .find(|r| {
                    r.device_kind == DeviceKind::Usb
                        && r.device_id == id
                        && r.port == i64::from(port)
                })
                .and_then(|r| frame.universes.get(&r.universe))
                .unwrap_or(&blank);
            if !write(soundswitch_frame(port, data)) {
                return Ok(());
            }
        }
        if let Some(rest) = FRAME_INTERVAL.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    // Black out on the way out, a few times over, so a dropped write does not
    // leave the rig lit.
    for _ in 0..3 {
        for port in 0..model.outputs {
            if !write(soundswitch_frame(port, &blank)) {
                return Ok(());
            }
        }
        std::thread::sleep(FRAME_INTERVAL);
    }
    Ok(())
}

/// Why a device would not open, in words that say what to do about it.
fn open_problem(error: &nusb::Error) -> String {
    if error.kind() == nusb::ErrorKind::PermissionDenied {
        if cfg!(target_os = "linux") {
            return "no permission to open it. Install Luma's udev rule (see gpui/BUILD.md) and replug it."
                .into();
        }
        return "no permission to open it.".into();
    }
    format!("could not open it: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_soundswitch_frame_is_the_header_then_the_universe() {
        let mut data = [0u8; 512];
        data[0] = 255;
        data[511] = 7;
        let packet = soundswitch_frame(1, &data);
        assert_eq!(packet.len(), 522);
        assert_eq!(&packet[..10], b"sTRt\x01\x00\x02\x02\x01\x00");
        assert_eq!(packet[10], 255);
        assert_eq!(packet[521], 7);
    }

    #[test]
    fn every_model_starts_with_twelve_byte_strt_packets() {
        for model in MODELS {
            assert!(!model.start.is_empty());
            assert!(model.start.iter().all(|p| p.starts_with(b"sTRt")));
        }
    }
}
