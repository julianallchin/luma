/// UDP port for device announcements, keepalives, and device number claim handshake.
pub const ANNOUNCE_PORT: u16 = 50000;
/// UDP port for precise position packets (CDJ-3000 broadcast at ~33 Hz).
pub const POSITION_PORT: u16 = 50001;
/// UDP port for CDJ status packets (unicast to us once we have a device number).
pub const STATUS_PORT: u16 = 50002;
/// TCP port on each CDJ for dbserver port discovery.
pub const DBSERVER_DISCOVERY_PORT: u16 = 12523;

/// Magic bytes that open every Pro DJ Link UDP packet.
pub const MAGIC: &[u8; 10] = b"Qspt1WmJOL";

// ── Packet type bytes (at offset 0x0a) ────────────────────────────────────────

pub const PKT_CLAIM_STAGE1: u8 = 0x00;
pub const PKT_CLAIM_STAGE2: u8 = 0x02;
pub const PKT_CLAIM_STAGE3: u8 = 0x04;
pub const PKT_KEEPALIVE: u8 = 0x06;
pub const PKT_IN_USE: u8 = 0x08;
/// 0x0a on port 50000 = hello during claim; same value on port 50002 = CDJ status.
pub const PKT_HELLO: u8 = 0x0a;
pub const PKT_CDJ_STATUS: u8 = 0x0a;
pub const PKT_PRECISE_POSITION: u8 = 0x0b;
pub const PKT_BEAT: u8 = 0x28;

// ── Keepalive device-type byte (0x21) ─────────────────────────────────────────

/// Older CDJ players (NXS, NXS2, XDJ-700, etc.)
pub const DEVICE_TYPE_CDJ: u8 = 0x01;
/// CDJ-3000, XDJ-XZ and newer Pioneer players
pub const DEVICE_TYPE_CDJ_3000: u8 = 0x03;
/// DJM mixers — excluded from deck tracking
pub const DEVICE_TYPE_MIXER: u8 = 0x02;

// ── CDJ status flag bits (byte at 0x89) ───────────────────────────────────────

pub const FLAG_ON_AIR: u8 = 0x08;
pub const FLAG_MASTER: u8 = 0x20;
pub const FLAG_PLAYING: u8 = 0x40;

// ── Slot IDs (byte at 0x29 in CDJ status) ────────────────────────────────────

pub const SLOT_USB: u8 = 0x03;
pub const SLOT_SD: u8 = 0x02;
pub const SLOT_CD: u8 = 0x01;

// ── Virtual CDJ identity ──────────────────────────────────────────────────────

/// Locally-administered fake MAC: 02:00:4c:55:4d:41 ("02:00:LUMA").
pub const OUR_MAC: [u8; 6] = [0x02, 0x00, 0x4c, 0x55, 0x4d, 0x41];
/// Device number claimed.  7 is safe (CDJ player slots are 1-4, mixer 33+).
pub const OUR_DEVICE_NUM: u8 = 7;
/// Device name shown on CDJ screens.
pub const OUR_DEVICE_NAME: &str = "Luma";
/// Keepalive broadcast interval.
pub const KEEPALIVE_INTERVAL_MS: u64 = 1500;
/// Inter-packet delay during the claim handshake.
pub const CLAIM_STEP_MS: u64 = 300;

// ── dbserver wire protocol constants ─────────────────────────────────────────

/// Start sentinel present in every dbserver message.
pub const DBSERVER_SENTINEL: u32 = 0x872349ae;

/// Tagged field type bytes.
pub const FIELD_U32: u8 = 0x11;
pub const FIELD_U16: u8 = 0x10;
pub const FIELD_U8: u8 = 0x0f;
pub const FIELD_BLOB: u8 = 0x14;
pub const FIELD_UTF16: u8 = 0x26;

/// dbserver message types.
pub const DBMSG_SETUP: u16 = 0x0000;
pub const DBMSG_TEARDOWN: u16 = 0x0100;
pub const DBMSG_METADATA_REQ: u16 = 0x2002;
pub const DBMSG_RENDER_REQ: u16 = 0x3000;
pub const DBMSG_MENU_AVAILABLE: u16 = 0x4000;
pub const DBMSG_MENU_HEADER: u16 = 0x4001;
pub const DBMSG_MENU_ITEM: u16 = 0x4101;
pub const DBMSG_MENU_FOOTER: u16 = 0x4201;

/// Response value indicating no results.
pub const DB_NO_RESULTS: u32 = 0xFFFFFFFF;

/// Menu item type constants (low 16 bits of args\[6\]).
pub const MENU_TITLE: u16 = 0x0004;
pub const MENU_ARTIST: u16 = 0x0007;
pub const MENU_DURATION: u16 = 0x000b;

/// Payload sent to port 12523 to discover the dbserver TCP port.
pub const DBSERVER_PORT_REQUEST: &[u8] = b"\x00\x00\x00\x0fRemoteDBServer\x00";

/// Track type for rekordbox tracks.
pub const TRACK_TYPE_REKORDBOX: u8 = 0x01;
/// Menu slot (always 1 in RMST).
pub const MENU_SLOT: u8 = 0x01;
