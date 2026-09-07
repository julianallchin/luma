use std::net::Ipv4Addr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

use crate::types::*;

// ── Public types ──────────────────────────────────────────────────────────────

pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub duration_secs: u32,
}

pub struct MetadataRequest {
    pub cdj_ip: Ipv4Addr,
    /// CDJ's own player number — used in the RMST field.
    pub player: u8,
    pub slot: u8,
    pub rekordbox_id: u32,
    /// The player number used as the key for matching results back to a deck.
    pub deck_key: u8,
}

pub struct MetadataResult {
    pub deck_key: u8,
    pub rekordbox_id: u32,
    pub title: String,
    pub artist: String,
    pub duration_secs: u32,
}

/// Fetch metadata for a track from the CDJ's dbserver.
/// Never fails — returns empty strings on any error.
pub async fn fetch_metadata(req: MetadataRequest) -> MetadataResult {
    let empty = MetadataResult {
        deck_key: req.deck_key,
        rekordbox_id: req.rekordbox_id,
        title: String::new(),
        artist: String::new(),
        duration_secs: 0,
    };

    match timeout(Duration::from_secs(8), do_fetch(&req)).await {
        Ok(Ok(meta)) => MetadataResult {
            deck_key: req.deck_key,
            rekordbox_id: req.rekordbox_id,
            title: meta.title,
            artist: meta.artist,
            duration_secs: meta.duration_secs,
        },
        _ => empty,
    }
}

// ── Wire encoding helpers ─────────────────────────────────────────────────────

fn tagged_u32(val: u32) -> Vec<u8> {
    let mut v = vec![FIELD_U32];
    v.extend_from_slice(&val.to_be_bytes());
    v
}

fn tagged_u16(val: u16) -> Vec<u8> {
    let mut v = vec![FIELD_U16];
    v.extend_from_slice(&val.to_be_bytes());
    v
}

fn tagged_u8(val: u8) -> Vec<u8> {
    vec![FIELD_U8, val]
}

fn tagged_blob(data: &[u8]) -> Vec<u8> {
    let mut v = vec![FIELD_BLOB];
    v.extend_from_slice(&(data.len() as u32).to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// Encode argument type tag bytes (12-byte blob, 0x06 = number4 argument).
fn arg_tags(tags: &[u8]) -> Vec<u8> {
    let mut padded = [0u8; 12];
    let len = tags.len().min(12);
    padded[..len].copy_from_slice(&tags[..len]);
    tagged_blob(&padded)
}

/// Build a full dbserver message.
fn make_message(txn: u32, mtype: u16, args: &[Vec<u8>], arg_type_tags: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend(tagged_u32(DBSERVER_SENTINEL));
    buf.extend(tagged_u32(txn));
    buf.extend(tagged_u16(mtype));
    buf.extend(tagged_u8(args.len() as u8));
    buf.extend(arg_tags(arg_type_tags));
    for arg in args {
        buf.extend_from_slice(arg);
    }
    buf
}

/// Encode RMST: requesting_player | menu(1) | slot | track_type
fn rmst(player: u8, slot: u8) -> Vec<u8> {
    let v: u32 = ((player as u32) << 24)
        | ((MENU_SLOT as u32) << 16)
        | ((slot as u32) << 8)
        | (TRACK_TYPE_REKORDBOX as u32);
    tagged_u32(v)
}

// ── Wire decoding helpers ─────────────────────────────────────────────────────

async fn read_exact(stream: &mut TcpStream, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Tagged field as returned by the server.
#[allow(dead_code)]
enum Field {
    U8(u8),
    U16(u16),
    U32(u32),
    Blob(Vec<u8>),
    Str(String),
}

async fn read_field(stream: &mut TcpStream) -> std::io::Result<Field> {
    let tag = read_exact(stream, 1).await?[0];
    match tag {
        FIELD_U8 => {
            let v = read_exact(stream, 1).await?[0];
            Ok(Field::U8(v))
        }
        FIELD_U16 => {
            let b = read_exact(stream, 2).await?;
            Ok(Field::U16(u16::from_be_bytes([b[0], b[1]])))
        }
        FIELD_U32 => {
            let b = read_exact(stream, 4).await?;
            Ok(Field::U32(u32::from_be_bytes([b[0], b[1], b[2], b[3]])))
        }
        FIELD_BLOB => {
            let lb = read_exact(stream, 4).await?;
            let len = u32::from_be_bytes([lb[0], lb[1], lb[2], lb[3]]) as usize;
            let data = read_exact(stream, len).await?;
            Ok(Field::Blob(data))
        }
        FIELD_UTF16 => {
            let cb = read_exact(stream, 4).await?;
            let char_count = u32::from_be_bytes([cb[0], cb[1], cb[2], cb[3]]) as usize;
            if char_count == 0 {
                return Ok(Field::Str(String::new()));
            }
            let raw = read_exact(stream, char_count * 2).await?;
            // Strip NUL terminator (last UTF-16 code unit)
            let char_count_no_nul = if char_count > 0 { char_count - 1 } else { 0 };
            let utf16: Vec<u16> = raw[..char_count_no_nul * 2]
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            let s = String::from_utf16_lossy(&utf16).to_string();
            Ok(Field::Str(s))
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unknown field tag: 0x{tag:02x}"),
        )),
    }
}

/// Read a complete message. Returns (mtype, txn, args).
async fn read_message(stream: &mut TcpStream) -> std::io::Result<(u16, u32, Vec<Field>)> {
    // sentinel
    let sentinel = match read_field(stream).await? {
        Field::U32(v) => v,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected sentinel u32",
            ))
        }
    };
    if sentinel != DBSERVER_SENTINEL {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad sentinel: 0x{sentinel:08x}"),
        ));
    }
    let txn = match read_field(stream).await? {
        Field::U32(v) => v,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected txn u32",
            ))
        }
    };
    let mtype = match read_field(stream).await? {
        Field::U16(v) => v,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected mtype u16",
            ))
        }
    };
    let argc = match read_field(stream).await? {
        Field::U8(v) => v as usize,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected argc u8",
            ))
        }
    };
    // arg type tags blob — discard
    read_field(stream).await?;
    let mut args = Vec::with_capacity(argc);
    for _ in 0..argc {
        args.push(read_field(stream).await?);
    }
    Ok((mtype, txn, args))
}

// ── Inner fetch (can return I/O errors) ───────────────────────────────────────

async fn do_fetch(req: &MetadataRequest) -> std::io::Result<TrackMetadata> {
    // ── Step 1: discover dbserver port ────────────────────────────────────────
    let db_port = {
        let mut stream = TcpStream::connect((req.cdj_ip, DBSERVER_DISCOVERY_PORT)).await?;
        stream.write_all(DBSERVER_PORT_REQUEST).await?;
        let b = read_exact(&mut stream, 2).await?;
        u16::from_be_bytes([b[0], b[1]])
    };

    // ── Step 2: connect to dbserver ───────────────────────────────────────────
    let mut stream = TcpStream::connect((req.cdj_ip, db_port)).await?;

    // ── Step 3: greeting — send num4(1), expect num4(1) back ─────────────────
    stream.write_all(&tagged_u32(1)).await?;
    read_field(&mut stream).await?; // discard echo

    // ── Step 4: SETUP ─────────────────────────────────────────────────────────
    // CDJ-3000: pose as the source player in both SETUP and RMST (matches beat-link behavior)
    let setup_msg = make_message(
        0xFFFF_FFFE,
        DBMSG_SETUP,
        &[tagged_u32(req.player as u32)],
        &[0x06],
    );
    stream.write_all(&setup_msg).await?;
    let (mtype, _, _) = read_message(&mut stream).await?;
    if mtype != DBMSG_MENU_AVAILABLE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected MENU_AVAILABLE after SETUP, got 0x{mtype:04x}"),
        ));
    }

    // ── Step 5: METADATA_REQ ──────────────────────────────────────────────────
    let meta_req = make_message(
        1,
        DBMSG_METADATA_REQ,
        &[rmst(req.player, req.slot), tagged_u32(req.rekordbox_id)],
        &[0x06, 0x06],
    );
    stream.write_all(&meta_req).await?;
    let (mtype, _, args) = read_message(&mut stream).await?;
    if mtype != DBMSG_MENU_AVAILABLE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected MENU_AVAILABLE after METADATA_REQ, got 0x{mtype:04x}"),
        ));
    }

    // args[1] = item count
    let count = match args.get(1) {
        Some(Field::U32(v)) => *v,
        _ => 0,
    };
    if count == 0 || count == DB_NO_RESULTS {
        send_teardown(&mut stream).await;
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "rekordbox_id not found",
        ));
    }

    // ── Step 6: RENDER_REQ ────────────────────────────────────────────────────
    let render_req = make_message(
        2,
        DBMSG_RENDER_REQ,
        &[
            rmst(req.player, req.slot),
            tagged_u32(0),     // offset
            tagged_u32(count), // limit
            tagged_u32(0),
            tagged_u32(count),
            tagged_u32(0),
        ],
        &[0x06, 0x06, 0x06, 0x06, 0x06, 0x06],
    );
    stream.write_all(&render_req).await?;

    // ── Step 7: read MENU_HEADER then items until MENU_FOOTER ─────────────────
    let (mtype, _, _) = read_message(&mut stream).await?;
    if mtype != DBMSG_MENU_HEADER {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected MENU_HEADER, got 0x{mtype:04x}"),
        ));
    }

    let mut title = String::new();
    let mut artist = String::new();
    let mut duration_secs = 0u32;

    loop {
        let (mtype, _, args) = read_message(&mut stream).await?;
        if mtype == DBMSG_MENU_FOOTER {
            break;
        }
        if mtype != DBMSG_MENU_ITEM {
            continue;
        }
        // item_type = low 16 bits of args[6]
        let item_type = match args.get(6) {
            Some(Field::U32(v)) => (*v & 0xFFFF) as u16,
            _ => continue,
        };
        match item_type {
            MENU_TITLE => {
                if let Some(Field::Str(s)) = args.get(3) {
                    title = s.clone();
                }
            }
            MENU_ARTIST => {
                if let Some(Field::Str(s)) = args.get(3) {
                    artist = s.clone();
                }
            }
            MENU_DURATION => {
                if let Some(Field::U32(v)) = args.get(1) {
                    duration_secs = *v;
                }
            }
            _ => {}
        }
    }

    send_teardown(&mut stream).await;
    Ok(TrackMetadata {
        title,
        artist,
        duration_secs,
    })
}

async fn send_teardown(stream: &mut TcpStream) {
    let msg = make_message(0xFFFF_FFFE, DBMSG_TEARDOWN, &[], &[]);
    let _ = stream.write_all(&msg).await;
}
