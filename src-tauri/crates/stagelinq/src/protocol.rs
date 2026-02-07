use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::{self, Cursor, Read};

/// Write a UTF-16 BE string: u32(byte_count) + u16 chars.
pub fn write_utf16_be(buf: &mut Vec<u8>, s: &str) {
    let chars: Vec<u16> = s.encode_utf16().collect();
    let byte_len = (chars.len() * 2) as u32;
    buf.write_u32::<BigEndian>(byte_len).unwrap();
    for ch in &chars {
        buf.write_u16::<BigEndian>(*ch).unwrap();
    }
}

/// Read a UTF-16 BE string: u32(byte_count) + u16 chars.
pub fn read_utf16_be(cursor: &mut Cursor<&[u8]>) -> io::Result<String> {
    let byte_len = cursor.read_u32::<BigEndian>()? as usize;
    if byte_len % 2 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UTF-16 byte length is not even",
        ));
    }
    let char_count = byte_len / 2;
    let mut chars = Vec::with_capacity(char_count);
    for _ in 0..char_count {
        chars.push(cursor.read_u16::<BigEndian>()?);
    }
    String::from_utf16(&chars)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Write raw fixed-size ASCII marker bytes (e.g. "airD", "smaa").
pub fn write_marker(buf: &mut Vec<u8>, marker: &[u8]) {
    buf.extend_from_slice(marker);
}

/// Read and verify a fixed-size ASCII marker.
pub fn read_marker(cursor: &mut Cursor<&[u8]>, expected: &[u8]) -> io::Result<()> {
    let mut got = vec![0u8; expected.len()];
    cursor.read_exact(&mut got)?;
    if got != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "expected marker {:?}, got {:?}",
                String::from_utf8_lossy(expected),
                String::from_utf8_lossy(&got)
            ),
        ));
    }
    Ok(())
}

/// Build a discovery message.
///
/// Format: "airD" + token(16B) + UTF16(source) + UTF16(action) + UTF16(name) + UTF16(version) + u16(port)
pub fn build_discovery_message(
    token: &[u8; 16],
    source: &str,
    action: &str,
    name: &str,
    version: &str,
    port: u16,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);
    write_marker(&mut buf, crate::types::DISCOVERY_MARKER);
    buf.extend_from_slice(token);
    write_utf16_be(&mut buf, source);
    write_utf16_be(&mut buf, action);
    write_utf16_be(&mut buf, name);
    write_utf16_be(&mut buf, version);
    buf.write_u16::<BigEndian>(port).unwrap();
    buf
}

/// Parsed discovery message.
#[derive(Debug, Clone)]
pub struct DiscoveryMessage {
    pub token: [u8; 16],
    pub source: String,
    pub action: String,
    pub software_name: String,
    pub software_version: String,
    pub port: u16,
}

/// Parse a discovery message from bytes.
pub fn parse_discovery_message(data: &[u8]) -> io::Result<DiscoveryMessage> {
    let mut cursor = Cursor::new(data);
    read_marker(&mut cursor, crate::types::DISCOVERY_MARKER)?;

    let mut token = [0u8; 16];
    cursor.read_exact(&mut token)?;

    let source = read_utf16_be(&mut cursor)?;
    let action = read_utf16_be(&mut cursor)?;
    let software_name = read_utf16_be(&mut cursor)?;
    let software_version = read_utf16_be(&mut cursor)?;
    let port = cursor.read_u16::<BigEndian>()?;

    Ok(DiscoveryMessage {
        token,
        source,
        action,
        software_name,
        software_version,
        port,
    })
}

/// Build a services request message for the main TCP connection.
///
/// Format: u32(MessageId::ServicesRequest) + token(16B)
pub fn build_services_request(token: &[u8; 16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(20);
    buf.write_u32::<BigEndian>(crate::types::MessageId::ServicesRequest as u32)
        .unwrap();
    buf.extend_from_slice(token);
    buf
}

/// Build a service announcement message (sent when connecting to a service port).
///
/// Format: u32(MessageId::ServicesAnnouncement) + token(16B) + UTF16(service_name) + u16(port)
pub fn build_service_announcement(token: &[u8; 16], service_name: &str, port: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.write_u32::<BigEndian>(crate::types::MessageId::ServicesAnnouncement as u32)
        .unwrap();
    buf.extend_from_slice(token);
    write_utf16_be(&mut buf, service_name);
    buf.write_u16::<BigEndian>(port).unwrap();
    buf
}

/// Parsed service announcement from the device's main TCP connection.
#[derive(Debug, Clone)]
pub struct ServiceAnnouncement {
    pub token: [u8; 16],
    pub service_name: String,
    pub port: u16,
}

/// Parse a TCP message from the main device connection.
/// Returns (message_id, remaining payload bytes).
pub fn parse_device_message(data: &[u8]) -> io::Result<(u32, [u8; 16], Vec<u8>)> {
    let mut cursor = Cursor::new(data);
    let msg_id = cursor.read_u32::<BigEndian>()?;
    let mut token = [0u8; 16];
    cursor.read_exact(&mut token)?;
    let mut payload = Vec::new();
    cursor.read_to_end(&mut payload)?;
    Ok((msg_id, token, payload))
}

/// Parse a service announcement payload (after message_id and token have been read).
pub fn parse_service_announcement_payload(payload: &[u8]) -> io::Result<(String, u16)> {
    let mut cursor = Cursor::new(payload);
    let service_name = read_utf16_be(&mut cursor)?;
    let port = cursor.read_u16::<BigEndian>()?;
    Ok((service_name, port))
}

/// Build a StateMap subscription message.
///
/// Wire format (length-prefixed — caller must wrap):
/// "smaa" + u32(0x7d2) + UTF16(path) + u32(0)
pub fn build_statemap_subscribe(path: &str) -> Vec<u8> {
    let mut inner = Vec::with_capacity(64);
    write_marker(&mut inner, crate::types::STATEMAP_MARKER);
    inner
        .write_u32::<BigEndian>(crate::types::STATEMAP_TYPE_INTERVAL)
        .unwrap();
    write_utf16_be(&mut inner, path);
    inner.write_u32::<BigEndian>(0).unwrap(); // update interval

    // Length-prefix the whole thing
    let mut buf = Vec::with_capacity(4 + inner.len());
    buf.write_u32::<BigEndian>(inner.len() as u32).unwrap();
    buf.extend_from_slice(&inner);
    buf
}

/// Parsed StateMap response value.
#[derive(Debug, Clone)]
pub enum StateMapValue {
    Json { name: String, value: serde_json::Value },
    Interval { name: String, interval: i32 },
}

/// Parse a StateMap response payload (after length prefix has been stripped).
pub fn parse_statemap_message(data: &[u8]) -> io::Result<StateMapValue> {
    let mut cursor = Cursor::new(data);
    read_marker(&mut cursor, crate::types::STATEMAP_MARKER)?;
    let msg_type = cursor.read_u32::<BigEndian>()?;

    match msg_type {
        crate::types::STATEMAP_TYPE_JSON => {
            let name = read_utf16_be(&mut cursor)?;
            let json_str = read_utf16_be(&mut cursor)?;
            let value: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("bad JSON: {e}"))
            })?;
            Ok(StateMapValue::Json { name, value })
        }
        crate::types::STATEMAP_TYPE_INTERVAL => {
            let name = read_utf16_be(&mut cursor)?;
            let interval = cursor.read_i32::<BigEndian>()?;
            Ok(StateMapValue::Interval { name, interval })
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown StateMap type: 0x{other:08x}"),
        )),
    }
}

/// Parsed beat info for one deck.
#[derive(Debug, Clone, Default)]
pub struct DeckBeatInfo {
    pub beat: f64,
    pub total_beats: f64,
    pub bpm: f64,
    pub samples: f64,
}

/// Parse a BeatInfo message payload (after length prefix has been stripped).
///
/// Format: u32(id) + u64(clock) + u32(deck_count) + [f64 beat + f64 totalBeats + f64 bpm] * deck_count + [f64 samples] * deck_count
pub fn parse_beat_info_message(data: &[u8]) -> io::Result<(u32, u64, Vec<DeckBeatInfo>)> {
    let mut cursor = Cursor::new(data);
    let id = cursor.read_u32::<BigEndian>()?;
    let clock = cursor.read_u64::<BigEndian>()?;
    let deck_count = cursor.read_u32::<BigEndian>()? as usize;

    let mut decks = Vec::with_capacity(deck_count);
    for _ in 0..deck_count {
        let beat = cursor.read_f64::<BigEndian>()?;
        let total_beats = cursor.read_f64::<BigEndian>()?;
        let bpm = cursor.read_f64::<BigEndian>()?;
        decks.push(DeckBeatInfo {
            beat,
            total_beats,
            bpm,
            samples: 0.0,
        });
    }
    // Read samples for each deck (may not be present if data is truncated)
    for deck in decks.iter_mut() {
        match cursor.read_f64::<BigEndian>() {
            Ok(s) => deck.samples = s,
            Err(_) => break,
        }
    }

    Ok((id, clock, decks))
}

/// BeatInfo subscription message (fixed 8 bytes).
pub fn build_beat_info_subscribe() -> Vec<u8> {
    vec![0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_round_trip() {
        let mut buf = Vec::new();
        write_utf16_be(&mut buf, "Hello");
        let mut cursor = Cursor::new(buf.as_slice());
        let result = read_utf16_be(&mut cursor).unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn utf16_empty_string() {
        let mut buf = Vec::new();
        write_utf16_be(&mut buf, "");
        let mut cursor = Cursor::new(buf.as_slice());
        let result = read_utf16_be(&mut cursor).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn utf16_unicode() {
        let mut buf = Vec::new();
        write_utf16_be(&mut buf, "DJ ♫ ☆");
        let mut cursor = Cursor::new(buf.as_slice());
        let result = read_utf16_be(&mut cursor).unwrap();
        assert_eq!(result, "DJ ♫ ☆");
    }

    #[test]
    fn discovery_message_round_trip() {
        let token = [1u8; 16];
        let msg = build_discovery_message(
            &token,
            "luma",
            "DISCOVERER_HOWDY_",
            "Luma",
            "0.1.0",
            0,
        );
        let parsed = parse_discovery_message(&msg).unwrap();
        assert_eq!(parsed.token, token);
        assert_eq!(parsed.source, "luma");
        assert_eq!(parsed.action, "DISCOVERER_HOWDY_");
        assert_eq!(parsed.software_name, "Luma");
        assert_eq!(parsed.software_version, "0.1.0");
        assert_eq!(parsed.port, 0);
    }

    #[test]
    fn statemap_subscribe_format() {
        let msg = build_statemap_subscribe("/Engine/Deck1/Play");
        // First 4 bytes are the length prefix
        let mut cursor = Cursor::new(msg.as_slice());
        let len = cursor.read_u32::<BigEndian>().unwrap() as usize;
        assert_eq!(len, msg.len() - 4);
    }

    #[test]
    fn statemap_json_parse() {
        // Build a mock StateMap JSON response
        let mut data = Vec::new();
        write_marker(&mut data, crate::types::STATEMAP_MARKER);
        data.write_u32::<BigEndian>(crate::types::STATEMAP_TYPE_JSON)
            .unwrap();
        write_utf16_be(&mut data, "/Engine/Deck1/Play");
        write_utf16_be(&mut data, r#"{"state":true}"#);

        let result = parse_statemap_message(&data).unwrap();
        match result {
            StateMapValue::Json { name, value } => {
                assert_eq!(name, "/Engine/Deck1/Play");
                assert_eq!(value["state"], true);
            }
            _ => panic!("expected JSON variant"),
        }
    }

    #[test]
    fn statemap_interval_parse() {
        let mut data = Vec::new();
        write_marker(&mut data, crate::types::STATEMAP_MARKER);
        data.write_u32::<BigEndian>(crate::types::STATEMAP_TYPE_INTERVAL)
            .unwrap();
        write_utf16_be(&mut data, "/Engine/Deck1/CurrentBPM");
        data.write_i32::<BigEndian>(128).unwrap();

        let result = parse_statemap_message(&data).unwrap();
        match result {
            StateMapValue::Interval { name, interval } => {
                assert_eq!(name, "/Engine/Deck1/CurrentBPM");
                assert_eq!(interval, 128);
            }
            _ => panic!("expected Interval variant"),
        }
    }

    #[test]
    fn beat_info_parse() {
        let mut data = Vec::new();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_u64::<BigEndian>(123456789).unwrap();
        data.write_u32::<BigEndian>(2).unwrap();
        data.write_f64::<BigEndian>(1.5).unwrap();
        data.write_f64::<BigEndian>(256.0).unwrap();
        data.write_f64::<BigEndian>(128.0).unwrap();
        data.write_f64::<BigEndian>(3.0).unwrap();
        data.write_f64::<BigEndian>(512.0).unwrap();
        data.write_f64::<BigEndian>(140.0).unwrap();
        data.write_f64::<BigEndian>(44100.0).unwrap();
        data.write_f64::<BigEndian>(88200.0).unwrap();

        let (id, clock, decks) = parse_beat_info_message(&data).unwrap();
        assert_eq!(id, 1);
        assert_eq!(clock, 123456789);
        assert_eq!(decks.len(), 2);
        assert!((decks[0].beat - 1.5).abs() < f64::EPSILON);
        assert!((decks[0].bpm - 128.0).abs() < f64::EPSILON);
        assert!((decks[0].samples - 44100.0).abs() < f64::EPSILON);
        assert!((decks[1].beat - 3.0).abs() < f64::EPSILON);
        assert!((decks[1].bpm - 140.0).abs() < f64::EPSILON);
        assert!((decks[1].samples - 88200.0).abs() < f64::EPSILON);
    }

    // --- UTF-16 byte-level verification (mirrors ReadContext/WriteContext tests) ---

    #[test]
    fn utf16_hi_exact_bytes() {
        let mut buf = Vec::new();
        write_utf16_be(&mut buf, "Hi");
        // 4 bytes length (0x00000004) + 'H' (0x0048) + 'i' (0x0069)
        assert_eq!(buf.len(), 8);
        assert_eq!(buf[0..4], [0, 0, 0, 4]);
        assert_eq!(buf[4..6], [0, 72]);  // 'H'
        assert_eq!(buf[6..8], [0, 105]); // 'i'
    }

    #[test]
    fn utf16_empty_exact_bytes() {
        let mut buf = Vec::new();
        write_utf16_be(&mut buf, "");
        assert_eq!(buf.len(), 4);
        assert_eq!(buf[3], 0); // length = 0
    }

    #[test]
    fn utf16_write_returns_correct_size() {
        let mut buf = Vec::new();
        write_utf16_be(&mut buf, "Test");
        // 4 bytes length + 4 chars * 2 bytes = 12
        assert_eq!(buf.len(), 12);
    }

    #[test]
    fn utf16_read_known_bytes() {
        // "Hi" in UTF-16 BE with length prefix
        let data: &[u8] = &[0, 0, 0, 4, 0, 72, 0, 105];
        let mut cursor = Cursor::new(data);
        let result = read_utf16_be(&mut cursor).unwrap();
        assert_eq!(result, "Hi");
    }

    // --- Discovery message tests (mirrors announce.test.ts) ---

    #[test]
    fn discovery_login_message() {
        let token = [0x42u8; 16];
        let msg = build_discovery_message(
            &token,
            "TestSource",
            "DISCOVERER_HOWDY_",
            "TestApp",
            "1.0.0",
            0,
        );
        let parsed = parse_discovery_message(&msg).unwrap();
        assert_eq!(parsed.action, "DISCOVERER_HOWDY_");
        assert_eq!(parsed.software_name, "TestApp");
        assert_eq!(parsed.software_version, "1.0.0");
        assert_eq!(parsed.source, "TestSource");
        assert_eq!(parsed.token, [0x42u8; 16]);
        assert_eq!(parsed.port, 0);
    }

    #[test]
    fn discovery_logout_message() {
        let token = [0u8; 16];
        let msg = build_discovery_message(
            &token,
            "NowPlaying",
            "DISCOVERER_EXIT_",
            "StageLinq",
            "2.0.0",
            0,
        );
        let parsed = parse_discovery_message(&msg).unwrap();
        assert_eq!(parsed.action, "DISCOVERER_EXIT_");
    }

    #[test]
    fn discovery_preserves_token() {
        let token: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let msg = build_discovery_message(&token, "Source", "TEST_ACTION", "Test", "1.0", 0);
        let parsed = parse_discovery_message(&msg).unwrap();
        assert_eq!(parsed.token[0], 1);
        assert_eq!(parsed.token[15], 16);
    }

    #[test]
    fn discovery_with_port() {
        let token = [0u8; 16];
        let msg = build_discovery_message(&token, "src", "LOGIN", "App", "1.0", 12345);
        let parsed = parse_discovery_message(&msg).unwrap();
        assert_eq!(parsed.port, 12345);
    }

    #[test]
    fn discovery_starts_with_aird_marker() {
        let token = [0u8; 16];
        let msg = build_discovery_message(&token, "s", "a", "n", "v", 0);
        assert_eq!(&msg[0..4], b"airD");
    }

    // --- BeatInfo additional tests (mirrors BeatInfo.test.ts) ---

    #[test]
    fn beat_info_single_deck() {
        let mut data = Vec::new();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_u64::<BigEndian>(0).unwrap();
        data.write_u32::<BigEndian>(1).unwrap(); // 1 deck
        data.write_f64::<BigEndian>(1.0).unwrap();
        data.write_f64::<BigEndian>(100.0).unwrap();
        data.write_f64::<BigEndian>(120.0).unwrap();
        data.write_f64::<BigEndian>(0.0).unwrap(); // samples

        let (_, _, decks) = parse_beat_info_message(&data).unwrap();
        assert_eq!(decks.len(), 1);
        assert!((decks[0].bpm - 120.0).abs() < f64::EPSILON);
    }

    #[test]
    fn beat_info_four_decks() {
        let mut data = Vec::new();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_u64::<BigEndian>(0).unwrap();
        data.write_u32::<BigEndian>(4).unwrap();
        for i in 0..4u32 {
            data.write_f64::<BigEndian>((i + 1) as f64).unwrap(); // beat
            data.write_f64::<BigEndian>(((i + 1) * 100) as f64).unwrap(); // total_beats
            data.write_f64::<BigEndian>((120 + i * 5) as f64).unwrap(); // bpm
        }
        for _ in 0..4 {
            data.write_f64::<BigEndian>(0.0).unwrap(); // samples
        }

        let (_, _, decks) = parse_beat_info_message(&data).unwrap();
        assert_eq!(decks.len(), 4);
        for i in 0..4 {
            assert!((decks[i].beat - (i + 1) as f64).abs() < f64::EPSILON);
            assert!((decks[i].total_beats - ((i + 1) * 100) as f64).abs() < f64::EPSILON);
            assert!((decks[i].bpm - (120 + i * 5) as f64).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn beat_info_zero_values() {
        let mut data = Vec::new();
        data.write_u32::<BigEndian>(0).unwrap();
        data.write_u64::<BigEndian>(0).unwrap();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_f64::<BigEndian>(0.0).unwrap();
        data.write_f64::<BigEndian>(0.0).unwrap();
        data.write_f64::<BigEndian>(0.0).unwrap();
        data.write_f64::<BigEndian>(0.0).unwrap();

        let (_, _, decks) = parse_beat_info_message(&data).unwrap();
        assert!((decks[0].beat).abs() < f64::EPSILON);
        assert!((decks[0].total_beats).abs() < f64::EPSILON);
        assert!((decks[0].bpm).abs() < f64::EPSILON);
    }

    #[test]
    fn beat_info_high_bpm() {
        let mut data = Vec::new();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_u64::<BigEndian>(0).unwrap();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_f64::<BigEndian>(1.0).unwrap();
        data.write_f64::<BigEndian>(100.0).unwrap();
        data.write_f64::<BigEndian>(200.0).unwrap();
        data.write_f64::<BigEndian>(0.0).unwrap();

        let (_, _, decks) = parse_beat_info_message(&data).unwrap();
        assert!((decks[0].bpm - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn beat_info_large_clock() {
        let mut data = Vec::new();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_u64::<BigEndian>(9_876_543_210_123_456_789).unwrap();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_f64::<BigEndian>(1.0).unwrap();
        data.write_f64::<BigEndian>(100.0).unwrap();
        data.write_f64::<BigEndian>(128.0).unwrap();
        data.write_f64::<BigEndian>(0.0).unwrap();

        let (_, clock, _) = parse_beat_info_message(&data).unwrap();
        assert_eq!(clock, 9_876_543_210_123_456_789);
    }

    #[test]
    fn beat_info_fractional_beats() {
        let mut data = Vec::new();
        data.write_u32::<BigEndian>(1).unwrap();
        data.write_u64::<BigEndian>(12345).unwrap();
        data.write_u32::<BigEndian>(2).unwrap();
        data.write_f64::<BigEndian>(1.5).unwrap();
        data.write_f64::<BigEndian>(256.0).unwrap();
        data.write_f64::<BigEndian>(128.0).unwrap();
        data.write_f64::<BigEndian>(3.25).unwrap();
        data.write_f64::<BigEndian>(512.0).unwrap();
        data.write_f64::<BigEndian>(140.0).unwrap();
        data.write_f64::<BigEndian>(44100.0).unwrap();
        data.write_f64::<BigEndian>(88200.0).unwrap();

        let (_, _, decks) = parse_beat_info_message(&data).unwrap();
        assert!((decks[0].beat - 1.5).abs() < f64::EPSILON);
        assert!((decks[1].beat - 3.25).abs() < f64::EPSILON);
        assert!((decks[0].samples - 44100.0).abs() < f64::EPSILON);
        assert!((decks[1].samples - 88200.0).abs() < f64::EPSILON);
    }

    // --- Marker tests ---

    #[test]
    fn marker_write_and_verify() {
        let mut buf = Vec::new();
        write_marker(&mut buf, b"airD");
        assert_eq!(&buf, &[0x61, 0x69, 0x72, 0x44]);
    }

    #[test]
    fn marker_read_valid() {
        let data: &[u8] = b"smaa";
        let mut cursor = Cursor::new(data);
        assert!(read_marker(&mut cursor, b"smaa").is_ok());
    }

    #[test]
    fn marker_read_invalid() {
        let data: &[u8] = b"xxxx";
        let mut cursor = Cursor::new(data);
        assert!(read_marker(&mut cursor, b"smaa").is_err());
    }

    // --- Service announcement round-trip ---

    #[test]
    fn service_announcement_round_trip() {
        let token = [0xABu8; 16];
        let msg = build_service_announcement(&token, "StateMap", 9000);
        let (msg_id, parsed_token, payload) = parse_device_message(&msg).unwrap();
        assert_eq!(msg_id, 0); // ServicesAnnouncement
        assert_eq!(parsed_token, token);
        let (name, port) = parse_service_announcement_payload(&payload).unwrap();
        assert_eq!(name, "StateMap");
        assert_eq!(port, 9000);
    }

    #[test]
    fn services_request_format() {
        let token = [0xFFu8; 16];
        let msg = build_services_request(&token);
        assert_eq!(msg.len(), 20); // 4 bytes msg_id + 16 bytes token
        // msg_id = 2 (ServicesRequest) in big-endian
        assert_eq!(msg[0..4], [0, 0, 0, 2]);
        assert_eq!(&msg[4..20], &token);
    }

    // --- StateMap additional tests ---

    #[test]
    fn statemap_json_string_value() {
        let mut data = Vec::new();
        write_marker(&mut data, crate::types::STATEMAP_MARKER);
        data.write_u32::<BigEndian>(crate::types::STATEMAP_TYPE_JSON).unwrap();
        write_utf16_be(&mut data, "/Engine/Deck1/Track/SongName");
        write_utf16_be(&mut data, r#"{"string":"My Song"}"#);

        let result = parse_statemap_message(&data).unwrap();
        match result {
            StateMapValue::Json { name, value } => {
                assert_eq!(name, "/Engine/Deck1/Track/SongName");
                assert_eq!(value["string"], "My Song");
            }
            _ => panic!("expected JSON variant"),
        }
    }

    #[test]
    fn statemap_json_numeric_value() {
        let mut data = Vec::new();
        write_marker(&mut data, crate::types::STATEMAP_MARKER);
        data.write_u32::<BigEndian>(crate::types::STATEMAP_TYPE_JSON).unwrap();
        write_utf16_be(&mut data, "/Engine/Deck1/CurrentBPM");
        write_utf16_be(&mut data, r#"{"value":128.5}"#);

        let result = parse_statemap_message(&data).unwrap();
        match result {
            StateMapValue::Json { name, value } => {
                assert_eq!(name, "/Engine/Deck1/CurrentBPM");
                assert!((value["value"].as_f64().unwrap() - 128.5).abs() < f64::EPSILON);
            }
            _ => panic!("expected JSON variant"),
        }
    }

    #[test]
    fn statemap_unknown_type_errors() {
        let mut data = Vec::new();
        write_marker(&mut data, crate::types::STATEMAP_MARKER);
        data.write_u32::<BigEndian>(0xDEADBEEF).unwrap();
        assert!(parse_statemap_message(&data).is_err());
    }

    // --- Mixed round-trip (mirrors WriteContext roundtrip test) ---

    #[test]
    fn mixed_write_read_round_trip() {
        let mut buf = Vec::new();
        buf.write_u32::<BigEndian>(12345).unwrap();
        buf.write_u16::<BigEndian>(6789).unwrap();
        write_utf16_be(&mut buf, "Hello");
        buf.write_u64::<BigEndian>(9876543210).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        assert_eq!(cursor.read_u32::<BigEndian>().unwrap(), 12345);
        assert_eq!(cursor.read_u16::<BigEndian>().unwrap(), 6789);
        assert_eq!(read_utf16_be(&mut cursor).unwrap(), "Hello");
        assert_eq!(cursor.read_u64::<BigEndian>().unwrap(), 9876543210);
    }

    // --- Endian-specific reads (mirrors ReadContext integer tests) ---

    #[test]
    fn read_u32_big_endian() {
        let data: &[u8] = &[0, 0, 1, 0]; // 256 in BE
        let mut cursor = Cursor::new(data);
        assert_eq!(cursor.read_u32::<BigEndian>().unwrap(), 256);
    }

    #[test]
    fn read_i32_positive() {
        let data: &[u8] = &[0, 0, 0, 127];
        let mut cursor = Cursor::new(data);
        assert_eq!(cursor.read_i32::<BigEndian>().unwrap(), 127);
    }

    #[test]
    fn read_i32_negative() {
        let data: &[u8] = &[0xff, 0xff, 0xff, 0xff];
        let mut cursor = Cursor::new(data);
        assert_eq!(cursor.read_i32::<BigEndian>().unwrap(), -1);
    }

    #[test]
    fn read_u16_big_endian() {
        let data: &[u8] = &[1, 0]; // 256 in BE
        let mut cursor = Cursor::new(data);
        assert_eq!(cursor.read_u16::<BigEndian>().unwrap(), 256);
    }

    #[test]
    fn read_u64_big_endian() {
        let data: &[u8] = &[0, 0, 0, 0, 0, 0, 1, 0]; // 256
        let mut cursor = Cursor::new(data);
        assert_eq!(cursor.read_u64::<BigEndian>().unwrap(), 256);
    }

    #[test]
    fn read_u64_large_value() {
        let data: &[u8] = &[0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]; // 0x100000000
        let mut cursor = Cursor::new(data);
        assert_eq!(cursor.read_u64::<BigEndian>().unwrap(), 0x100000000);
    }

    #[test]
    fn read_f64_big_endian() {
        // IEEE 754: 1.5 in big endian
        let data: &[u8] = &[0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut cursor = Cursor::new(data);
        assert!((cursor.read_f64::<BigEndian>().unwrap() - 1.5).abs() < f64::EPSILON);
    }

    // --- Beat info subscribe message ---

    #[test]
    fn beat_info_subscribe_format() {
        let msg = build_beat_info_subscribe();
        assert_eq!(msg, vec![0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00]);
    }
}
