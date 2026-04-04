use std::net::Ipv4Addr;

use crate::types::*;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Write a NUL-padded ASCII device name into a 20-byte slice.
fn name_bytes(name: &str) -> [u8; 20] {
    let mut buf = [0u8; 20];
    let bytes = name.as_bytes();
    let len = bytes.len().min(20);
    buf[..len].copy_from_slice(&bytes[..len]);
    buf
}

/// Write magic + type byte + NUL subtype at the start of a packet buffer.
fn write_header(buf: &mut [u8], pkt_type: u8) {
    buf[0..10].copy_from_slice(MAGIC);
    buf[0x0a] = pkt_type;
    buf[0x0b] = 0x00;
}

/// Check the magic header and minimum length.
fn check(data: &[u8], min_len: usize, expected_type: u8) -> bool {
    data.len() >= min_len && data[0..10] == *MAGIC && data[0x0a] == expected_type
}

fn read_be_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn read_be_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

// ── Packet builders ───────────────────────────────────────────────────────────

/// Build the CDJ hello packet (38 bytes) sent 3× before the claim.
pub fn build_hello() -> [u8; 38] {
    let mut buf = [0u8; 38];
    write_header(&mut buf, PKT_HELLO);
    buf[0x0c..0x20].copy_from_slice(&name_bytes(OUR_DEVICE_NAME));
    buf[0x20] = 0x01;
    buf[0x21] = 0x04;
    buf[0x22] = 0x00;
    buf[0x23] = 0x26; // length = 38
    buf[0x24] = 0x01;
    buf[0x25] = 0x40;
    buf
}

/// Build claim stage-1 packet (44 bytes). `counter` is 1, 2, or 3.
pub fn build_stage1(counter: u8) -> [u8; 44] {
    let mut buf = [0u8; 44];
    write_header(&mut buf, PKT_CLAIM_STAGE1);
    buf[0x0c..0x20].copy_from_slice(&name_bytes(OUR_DEVICE_NAME));
    buf[0x20] = 0x01;
    buf[0x21] = 0x03;
    buf[0x22] = 0x00;
    buf[0x23] = 0x2c; // length = 44
    buf[0x24] = counter;
    buf[0x25] = 0x01;
    buf[0x26..0x2c].copy_from_slice(&OUR_MAC);
    buf
}

/// Build claim stage-2 packet (50 bytes). `our_ip` is our LAN address.
pub fn build_stage2(our_ip: Ipv4Addr, counter: u8, device_num: u8) -> [u8; 50] {
    let mut buf = [0u8; 50];
    write_header(&mut buf, PKT_CLAIM_STAGE2);
    buf[0x0c..0x20].copy_from_slice(&name_bytes(OUR_DEVICE_NAME));
    buf[0x20] = 0x01;
    buf[0x21] = 0x03;
    buf[0x22] = 0x00;
    buf[0x23] = 0x32; // length = 50
    buf[0x24..0x28].copy_from_slice(&our_ip.octets());
    buf[0x28..0x2e].copy_from_slice(&OUR_MAC);
    buf[0x2e] = device_num;
    buf[0x2f] = counter;
    buf[0x30] = 0x0d;
    buf[0x31] = 0x01; // auto-assign
    buf
}

/// Build claim stage-3 packet (38 bytes).
pub fn build_stage3(counter: u8, device_num: u8) -> [u8; 38] {
    let mut buf = [0u8; 38];
    write_header(&mut buf, PKT_CLAIM_STAGE3);
    buf[0x0c..0x20].copy_from_slice(&name_bytes(OUR_DEVICE_NAME));
    buf[0x20] = 0x01;
    buf[0x21] = 0x03;
    buf[0x22] = 0x00;
    buf[0x23] = 0x26; // length = 38
    buf[0x24] = device_num;
    buf[0x25] = counter;
    buf
}

/// Build our keepalive broadcast packet (54 bytes). `peer_count` = known CDJ count.
pub fn build_keepalive(our_ip: Ipv4Addr, peer_count: u8, device_num: u8) -> [u8; 54] {
    let mut buf = [0u8; 54];
    write_header(&mut buf, PKT_KEEPALIVE);
    buf[0x0c..0x20].copy_from_slice(&name_bytes(OUR_DEVICE_NAME));
    buf[0x20] = 0x01;
    buf[0x21] = DEVICE_TYPE_CDJ;
    buf[0x22] = 0x00;
    buf[0x23] = 0x36; // length = 54
    buf[0x24] = device_num;
    buf[0x25] = 0x01;
    buf[0x26..0x2c].copy_from_slice(&OUR_MAC);
    buf[0x2c..0x30].copy_from_slice(&our_ip.octets());
    buf[0x30] = peer_count;
    buf[0x31] = 0x00;
    buf[0x32] = 0x00;
    buf[0x33] = 0x00;
    buf[0x34] = 0x01;
    buf[0x35] = 0x64;
    buf
}

// ── Parsed packet types ───────────────────────────────────────────────────────

/// A CDJ keepalive announcement parsed from port 50000.
pub struct KeepalivePacket {
    pub name: String,
    pub device_type: u8,
    pub player: u8,
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
}

/// Parsed CDJ status packet from port 50002.
pub struct StatusPacket {
    pub player: u8,
    pub slot: u8,
    pub rekordbox_id: u32,
    pub flags: u8,
    /// Raw 3-byte big-endian pitch at offset 0x8d.
    pub pitch_raw3: [u8; 3],
    /// BPM × 100 at offset 0x92 (0xffff = not available).
    pub bpm_x100: u16,
    /// Beat number at offset 0xa0.
    pub beat_number: u32,
    /// Beat-within-bar (1-4) at offset 0xa6.
    pub beat_within_bar: u8,
}

impl StatusPacket {
    /// Pitch multiplier from the 3-byte raw value (neutral = 0x100000).
    pub fn pitch_multiplier(&self) -> f64 {
        let raw = u32::from_be_bytes([
            0,
            self.pitch_raw3[0],
            self.pitch_raw3[1],
            self.pitch_raw3[2],
        ]);
        raw as f64 / 0x100000_u32 as f64
    }

    /// Track BPM (base, before pitch adjustment).
    pub fn bpm(&self) -> f64 {
        if self.bpm_x100 == 0xffff {
            0.0
        } else {
            self.bpm_x100 as f64 / 100.0
        }
    }

    /// Effective (pitch-adjusted) BPM.
    pub fn effective_bpm(&self) -> f64 {
        self.bpm() * self.pitch_multiplier()
    }
}

/// Parsed precise position packet (CDJ-3000) from port 50001.
pub struct PositionPacket {
    pub player: u8,
    pub track_length_s: u32,
    pub position_ms: u32,
    /// Pitch percentage × 100 (signed; e.g. 600 = +6%).
    pub pitch_pct_x100: i32,
    /// Effective BPM × 10 (already pitch-adjusted).
    pub bpm_x10: u32,
}

impl PositionPacket {
    pub fn pitch_multiplier(&self) -> f64 {
        1.0 + (self.pitch_pct_x100 as f64 / 10000.0)
    }

    pub fn effective_bpm(&self) -> f64 {
        self.bpm_x10 as f64 / 10.0
    }
}

// ── Packet parsers ────────────────────────────────────────────────────────────

/// Parse a keepalive packet from port 50000. Returns `None` if malformed.
pub fn parse_keepalive(data: &[u8]) -> Option<KeepalivePacket> {
    if !check(data, 54, PKT_KEEPALIVE) {
        return None;
    }
    let name = String::from_utf8_lossy(&data[0x0c..0x20])
        .trim_end_matches('\0')
        .to_string();
    let device_type = data[0x21];
    let player = data[0x24];
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&data[0x26..0x2c]);
    let ip = Ipv4Addr::new(data[0x2c], data[0x2d], data[0x2e], data[0x2f]);
    Some(KeepalivePacket {
        name,
        device_type,
        player,
        mac,
        ip,
    })
}

/// Parse a CDJ status packet from port 50002. Returns `None` if malformed.
/// `our_device_num` is used to filter out our own reflected packets.
pub fn parse_status(data: &[u8], our_device_num: u8) -> Option<StatusPacket> {
    // CDJ-3000 status packets are 0xCC (204) bytes; older CDJs can be longer
    if !check(data, 204, PKT_CDJ_STATUS) {
        return None;
    }
    let player = data[0x21];
    // Skip our own reflections and mixer status
    if player == our_device_num {
        return None;
    }
    let slot = data[0x29];
    let rekordbox_id = read_be_u32(data, 0x2c);
    let flags = data[0x89];
    let pitch_raw3 = [data[0x8d], data[0x8e], data[0x8f]];
    let bpm_x100 = read_be_u16(data, 0x92);
    let beat_number_raw = read_be_u32(data, 0xa0);
    let beat_number = if beat_number_raw == 0xffff_ffff {
        0
    } else {
        beat_number_raw
    };
    let beat_within_bar = data[0xa6];
    Some(StatusPacket {
        player,
        slot,
        rekordbox_id,
        flags,
        pitch_raw3,
        bpm_x100,
        beat_number,
        beat_within_bar,
    })
}

/// Parse a precise position packet from port 50001. Returns `None` if malformed.
/// `our_device_num` is used to filter out our own reflected packets.
pub fn parse_position(data: &[u8], our_device_num: u8) -> Option<PositionPacket> {
    if !check(data, 60, PKT_PRECISE_POSITION) {
        return None;
    }
    let player = data[0x21];
    if player == our_device_num {
        return None;
    }
    let track_length_s = read_be_u32(data, 0x24);
    let position_ms = read_be_u32(data, 0x28);
    // Signed pitch percentage × 100 stored as a 32-bit big-endian integer
    let pitch_pct_x100 = i32::from_be_bytes([data[0x2c], data[0x2d], data[0x2e], data[0x2f]]);
    let bpm_x10 = read_be_u32(data, 0x38);
    Some(PositionPacket {
        player,
        track_length_s,
        position_ms,
        pitch_pct_x100,
        bpm_x10,
    })
}
