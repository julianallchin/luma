//! Physical encodings for artifacts.
//!
//! One manifest does not mean one physical encoding (design §9.3). Existing
//! caches are handed to Python as-is — `.pcm` keeps its 18-byte header, MERT
//! keeps its `.npy` — and freshly materialized Rust vectors go out as headerless
//! little-endian blocks. Every codec here answers the same two questions: where
//! do the numbers start, and what shape/dtype are they.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::agent_execution::bindings::manifest::DType;
use crate::agent_execution::error::{err, Result};

// ---------------------------------------------------------------------------
// raw_le — headerless contiguous little-endian
// ---------------------------------------------------------------------------

pub mod raw_le {
    use super::*;

    /// Write `data` as headerless little-endian f32, returning the byte length.
    pub fn write_f32(path: &Path, data: &[f32]) -> Result<u64> {
        let mut w = BufWriter::new(File::create(path)?);
        for v in data {
            w.write_all(&v.to_le_bytes())?;
        }
        w.flush()?;
        Ok((data.len() * 4) as u64)
    }

    /// Write `data` as headerless little-endian f64, returning the byte length.
    pub fn write_f64(path: &Path, data: &[f64]) -> Result<u64> {
        let mut w = BufWriter::new(File::create(path)?);
        for v in data {
            w.write_all(&v.to_le_bytes())?;
        }
        w.flush()?;
        Ok((data.len() * 8) as u64)
    }

    /// Write `data` as headerless little-endian i64, returning the byte length.
    pub fn write_i64(path: &Path, data: &[i64]) -> Result<u64> {
        let mut w = BufWriter::new(File::create(path)?);
        for v in data {
            w.write_all(&v.to_le_bytes())?;
        }
        w.flush()?;
        Ok((data.len() * 8) as u64)
    }

    /// Read back headerless little-endian f32 (test/debug helper).
    pub fn read_f32(path: &Path, byte_offset: u64, count: usize) -> Result<Vec<f32>> {
        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(byte_offset))?;
        let mut buf = vec![0u8; count * 4];
        f.read_exact(&mut buf)?;
        Ok(buf
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// npy — minimal NumPy v1.0/v2.0 reader + v1.0 writer
// ---------------------------------------------------------------------------

const NPY_MAGIC: &[u8; 6] = b"\x93NUMPY";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpyHeader {
    pub dtype: DType,
    pub shape: Vec<usize>,
    /// Byte offset at which the array data starts.
    pub data_offset: u64,
    pub major: u8,
    pub minor: u8,
}

impl NpyHeader {
    pub fn element_count(&self) -> u64 {
        self.shape.iter().fold(1u64, |acc, d| acc * (*d as u64))
    }

    pub fn data_len(&self) -> u64 {
        self.element_count() * self.dtype.size_bytes()
    }
}

/// Parse the header of an existing `.npy` file. Fortran order is rejected —
/// every consumer downstream assumes C-order contiguity.
pub fn read_npy_header(path: &Path) -> Result<NpyHeader> {
    let mut f = File::open(path)?;
    let mut prefix = [0u8; 10];
    read_exact_or(&mut f, &mut prefix, "npy file is shorter than its magic")?;
    if &prefix[..6] != NPY_MAGIC {
        return err("not an npy file: bad magic");
    }
    let major = prefix[6];
    let minor = prefix[7];
    let (header_len, len_field) = match major {
        1 => (u16::from_le_bytes([prefix[8], prefix[9]]) as usize, 2usize),
        2 | 3 => {
            let mut rest = [0u8; 2];
            read_exact_or(&mut f, &mut rest, "truncated npy header length")?;
            (
                u32::from_le_bytes([prefix[8], prefix[9], rest[0], rest[1]]) as usize,
                4usize,
            )
        }
        other => return err(format!("unsupported npy version {other}.{minor}")),
    };
    let mut dict = vec![0u8; header_len];
    read_exact_or(&mut f, &mut dict, "truncated npy header dict")?;
    let dict = String::from_utf8(dict).map_err(|_| {
        crate::agent_execution::error::DataPlaneError::new("npy header is not valid utf-8")
    })?;

    let descr = extract_quoted(&dict, "'descr'").ok_or_else(|| dpe("npy header missing descr"))?;
    let dtype = DType::from_npy_descr(&descr)?;

    let fortran = extract_after(&dict, "'fortran_order'")
        .ok_or_else(|| dpe("npy header missing fortran_order"))?;
    if fortran.starts_with("True") {
        return err("fortran-ordered npy files are not supported");
    }

    let shape = parse_shape(&dict).ok_or_else(|| dpe("npy header missing shape"))?;

    let data_offset = (6 + 2 + len_field + header_len) as u64;
    let header = NpyHeader {
        dtype,
        shape,
        data_offset,
        major,
        minor,
    };
    let actual = f.metadata()?.len();
    let expected = data_offset + header.data_len();
    if actual < expected {
        return err(format!(
            "truncated npy file: {actual} bytes, expected at least {expected}"
        ));
    }
    Ok(header)
}

/// Write a C-order v1.0 `.npy`, returning the total byte length.
pub fn write_npy_f32(path: &Path, data: &[f32], shape: &[usize]) -> Result<u64> {
    write_npy(path, DType::F32, shape, data.len(), |w| {
        for v in data {
            w.write_all(&v.to_le_bytes())?;
        }
        Ok(())
    })
}

/// Write a C-order v1.0 `.npy`, returning the total byte length.
pub fn write_npy_f64(path: &Path, data: &[f64], shape: &[usize]) -> Result<u64> {
    write_npy(path, DType::F64, shape, data.len(), |w| {
        for v in data {
            w.write_all(&v.to_le_bytes())?;
        }
        Ok(())
    })
}

fn write_npy<F>(
    path: &Path,
    dtype: DType,
    shape: &[usize],
    len: usize,
    write_data: F,
) -> Result<u64>
where
    F: FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
{
    let expected: usize = shape.iter().product::<usize>();
    if expected != len {
        return err(format!(
            "npy shape {shape:?} implies {expected} elements but {len} were given"
        ));
    }
    let shape_str = if shape.len() == 1 {
        format!("({},)", shape[0])
    } else {
        format!(
            "({})",
            shape
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut dict = format!(
        "{{'descr': '{}', 'fortran_order': False, 'shape': {}, }}",
        dtype.npy_descr(),
        shape_str
    );
    // The data must start on a 64-byte boundary; pad the dict with spaces and
    // terminate it with a newline, exactly as numpy does.
    let unpadded = 10 + dict.len() + 1;
    let padding = (64 - (unpadded % 64)) % 64;
    dict.push_str(&" ".repeat(padding));
    dict.push('\n');

    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(NPY_MAGIC)?;
    w.write_all(&[1u8, 0u8])?;
    w.write_all(&(dict.len() as u16).to_le_bytes())?;
    w.write_all(dict.as_bytes())?;
    write_data(&mut w)?;
    w.flush()?;
    Ok((10 + dict.len()) as u64 + (len as u64) * dtype.size_bytes())
}

fn parse_shape(dict: &str) -> Option<Vec<usize>> {
    let rest = extract_after(dict, "'shape'")?;
    let open = rest.find('(')?;
    let close = rest[open..].find(')')? + open;
    let inner = &rest[open + 1..close];
    let mut dims = Vec::new();
    for part in inner.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        dims.push(p.parse::<usize>().ok()?);
    }
    Some(dims)
}

fn extract_after<'a>(dict: &'a str, key: &str) -> Option<&'a str> {
    let idx = dict.find(key)? + key.len();
    let rest = dict[idx..].trim_start();
    let rest = rest.strip_prefix(':')?;
    Some(rest.trim_start())
}

fn extract_quoted(dict: &str, key: &str) -> Option<String> {
    let rest = extract_after(dict, key)?;
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let end = rest[1..].find(quote)? + 1;
    Some(rest[1..end].to_string())
}

// ---------------------------------------------------------------------------
// pcm_f32 — Luma's existing 18-byte-header audio cache
// ---------------------------------------------------------------------------

/// `version u32 LE | sample_rate u32 LE | channels u16 LE | len u64 LE`, then
/// `len` interleaved `f32 LE` samples.
pub const PCM_HEADER_LEN: u64 = 18;

/// Versions Luma has ever written. `audio/cache.rs` writes 2; the eval mono
/// writer historically wrote 1 and its reader ignored the field entirely, so
/// both must be accepted on read.
pub const PCM_SUPPORTED_VERSIONS: [u32; 2] = [1, 2];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmHeader {
    pub version: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Frames, i.e. interleaved sample count divided by channels.
    pub frames: u64,
    /// Total interleaved sample count, as stored in the header.
    pub samples: u64,
    pub data_offset: u64,
}

impl PcmHeader {
    pub fn data_len(&self) -> u64 {
        self.samples * 4
    }

    /// Canonical tensor shape: `[frames]` for mono, `[frames, channels]` for
    /// interleaved multichannel (row-major = raw layout).
    pub fn tensor_shape(&self) -> Vec<usize> {
        if self.channels <= 1 {
            vec![self.frames as usize]
        } else {
            vec![self.frames as usize, self.channels as usize]
        }
    }
}

pub fn read_pcm_header(path: &Path) -> Result<PcmHeader> {
    let mut f = File::open(path)?;
    let mut buf = [0u8; PCM_HEADER_LEN as usize];
    read_exact_or(
        &mut f,
        &mut buf,
        "pcm file is shorter than its 18-byte header",
    )?;
    let header = parse_pcm_header(&buf)?;
    let actual = f.metadata()?.len();
    let expected = PCM_HEADER_LEN + header.data_len();
    if actual < expected {
        return err(format!(
            "truncated pcm file: {actual} bytes, header declares {expected}"
        ));
    }
    if !(actual - PCM_HEADER_LEN).is_multiple_of(4) {
        return err(format!(
            "pcm payload is not a whole number of f32 samples ({} bytes)",
            actual - PCM_HEADER_LEN
        ));
    }
    Ok(header)
}

pub fn parse_pcm_header(buf: &[u8]) -> Result<PcmHeader> {
    if buf.len() < PCM_HEADER_LEN as usize {
        return err("pcm header is shorter than 18 bytes");
    }
    let version = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if !PCM_SUPPORTED_VERSIONS.contains(&version) {
        return err(format!("unsupported pcm cache version {version}"));
    }
    let sample_rate = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if sample_rate == 0 {
        return err("pcm header declares a zero sample rate");
    }
    let channels = u16::from_le_bytes([buf[8], buf[9]]);
    if channels == 0 {
        return err("pcm header declares zero channels");
    }
    let samples = u64::from_le_bytes([
        buf[10], buf[11], buf[12], buf[13], buf[14], buf[15], buf[16], buf[17],
    ]);
    if !samples.is_multiple_of(channels as u64) {
        return err(format!(
            "pcm sample count {samples} is not divisible by {channels} channels"
        ));
    }
    Ok(PcmHeader {
        version,
        sample_rate,
        channels,
        frames: samples / channels as u64,
        samples,
        data_offset: PCM_HEADER_LEN,
    })
}

/// Write a pcm_f32 file (test fixture / fresh-materialization helper).
pub fn write_pcm(
    path: &Path,
    version: u32,
    sample_rate: u32,
    channels: u16,
    samples: &[f32],
) -> Result<u64> {
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(&version.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&(samples.len() as u64).to_le_bytes())?;
    for v in samples {
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()?;
    Ok(PCM_HEADER_LEN + (samples.len() * 4) as u64)
}

// ---------------------------------------------------------------------------
// png — passthrough, dimensions only
// ---------------------------------------------------------------------------

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngInfo {
    pub width: u32,
    pub height: u32,
    pub byte_len: u64,
}

/// Validate the magic and read `IHDR`'s dimensions. The pixels are never
/// decoded — figures pass through the store untouched.
pub fn read_png_info(path: &Path) -> Result<PngInfo> {
    let mut f = File::open(path)?;
    let mut head = [0u8; 24];
    read_exact_or(&mut f, &mut head, "png file is shorter than its IHDR")?;
    if head[..8] != PNG_MAGIC {
        return err("not a png file: bad magic");
    }
    if &head[12..16] != b"IHDR" {
        return err("png file does not start with an IHDR chunk");
    }
    Ok(PngInfo {
        width: u32::from_be_bytes([head[16], head[17], head[18], head[19]]),
        height: u32::from_be_bytes([head[20], head[21], head[22], head[23]]),
        byte_len: f.metadata()?.len(),
    })
}

// ---------------------------------------------------------------------------

fn dpe(msg: &str) -> crate::agent_execution::error::DataPlaneError {
    crate::agent_execution::error::DataPlaneError::new(msg)
}

fn read_exact_or(f: &mut File, buf: &mut [u8], msg: &str) -> Result<()> {
    f.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            dpe(msg)
        } else {
            e.into()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn raw_le_round_trips_f32() {
        let d = dir();
        let p = d.path().join("x.bin");
        let data = [1.0f32, -2.5, 3.25, 0.0];
        let len = raw_le::write_f32(&p, &data).unwrap();
        assert_eq!(len, 16);
        assert_eq!(fs::metadata(&p).unwrap().len(), 16);
        assert_eq!(raw_le::read_f32(&p, 0, 4).unwrap(), data);
        assert_eq!(raw_le::read_f32(&p, 8, 2).unwrap(), [3.25, 0.0]);
    }

    #[test]
    fn raw_le_writes_little_endian_bytes() {
        let d = dir();
        let p = d.path().join("x.bin");
        raw_le::write_f64(&p, &[1.0f64]).unwrap();
        assert_eq!(fs::read(&p).unwrap(), vec![0, 0, 0, 0, 0, 0, 0xf0, 0x3f]);
        let p2 = d.path().join("y.bin");
        raw_le::write_i64(&p2, &[1i64]).unwrap();
        assert_eq!(fs::read(&p2).unwrap(), vec![1, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn npy_round_trips_f32_1d() {
        let d = dir();
        let p = d.path().join("a.npy");
        let data = [1.0f32, 2.0, 3.0];
        let total = write_npy_f32(&p, &data, &[3]).unwrap();
        assert_eq!(total, fs::metadata(&p).unwrap().len());
        let h = read_npy_header(&p).unwrap();
        assert_eq!(h.dtype, DType::F32);
        assert_eq!(h.shape, vec![3]);
        assert_eq!(h.data_offset % 64, 0);
        assert_eq!(h.data_len(), 12);
        assert_eq!(raw_le::read_f32(&p, h.data_offset, 3).unwrap(), data);
    }

    #[test]
    fn npy_round_trips_f64_2d() {
        let d = dir();
        let p = d.path().join("a.npy");
        let data: Vec<f64> = (0..6).map(|i| i as f64).collect();
        write_npy_f64(&p, &data, &[2, 3]).unwrap();
        let h = read_npy_header(&p).unwrap();
        assert_eq!(h.dtype, DType::F64);
        assert_eq!(h.shape, vec![2, 3]);
        assert_eq!(h.element_count(), 6);
        assert_eq!(h.data_len(), 48);
    }

    #[test]
    fn npy_shape_must_match_data_length() {
        let d = dir();
        let p = d.path().join("a.npy");
        let e = write_npy_f32(&p, &[1.0, 2.0], &[3]).unwrap_err();
        assert!(e.message().contains("implies 3 elements"), "{e}");
    }

    #[test]
    fn npy_header_matches_numpy_layout() {
        let d = dir();
        let p = d.path().join("a.npy");
        write_npy_f32(&p, &[1.0], &[1]).unwrap();
        let bytes = fs::read(&p).unwrap();
        assert_eq!(&bytes[..6], NPY_MAGIC);
        assert_eq!(bytes[6], 1);
        assert_eq!(bytes[7], 0);
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let dict = std::str::from_utf8(&bytes[10..10 + header_len]).unwrap();
        assert!(dict.starts_with("{'descr': '<f4', 'fortran_order': False, 'shape': (1,), }"));
        assert!(dict.ends_with('\n'));
        assert_eq!((10 + header_len) % 64, 0);
    }

    fn write_raw_npy(path: &Path, dict: &str, data: &[u8]) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(NPY_MAGIC);
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&(dict.len() as u16).to_le_bytes());
        bytes.extend_from_slice(dict.as_bytes());
        bytes.extend_from_slice(data);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn npy_rejects_fortran_order() {
        let d = dir();
        let p = d.path().join("f.npy");
        write_raw_npy(
            &p,
            "{'descr': '<f4', 'fortran_order': True, 'shape': (2, 2), }\n",
            &[0u8; 16],
        );
        let e = read_npy_header(&p).unwrap_err();
        assert!(e.message().contains("fortran"), "{e}");
    }

    #[test]
    fn npy_rejects_truncated_data() {
        let d = dir();
        let p = d.path().join("t.npy");
        write_raw_npy(
            &p,
            "{'descr': '<f4', 'fortran_order': False, 'shape': (16,), }\n",
            &[0u8; 8],
        );
        let e = read_npy_header(&p).unwrap_err();
        assert!(e.message().contains("truncated npy file"), "{e}");
    }

    #[test]
    fn npy_rejects_truncated_header_and_bad_magic() {
        let d = dir();
        let p = d.path().join("short.npy");
        fs::write(&p, b"\x93NUM").unwrap();
        assert!(read_npy_header(&p).unwrap_err().message().contains("magic"));

        let p2 = d.path().join("bad.npy");
        fs::write(&p2, b"not an npy file at all").unwrap();
        assert!(read_npy_header(&p2)
            .unwrap_err()
            .message()
            .contains("bad magic"));

        let p3 = d.path().join("cut.npy");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(NPY_MAGIC);
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&(120u16).to_le_bytes());
        bytes.extend_from_slice(b"{'descr'");
        fs::write(&p3, bytes).unwrap();
        assert!(read_npy_header(&p3)
            .unwrap_err()
            .message()
            .contains("truncated npy header dict"));
    }

    #[test]
    fn npy_rejects_unsupported_dtypes() {
        let d = dir();
        let p = d.path().join("u.npy");
        write_raw_npy(
            &p,
            "{'descr': '<u1', 'fortran_order': False, 'shape': (4,), }\n",
            &[0u8; 4],
        );
        let e = read_npy_header(&p).unwrap_err();
        assert!(e.message().contains("unsupported npy dtype"), "{e}");
    }

    #[test]
    fn npy_reads_fp16_mert_style_headers() {
        let d = dir();
        let p = d.path().join("mert.npy");
        write_raw_npy(
            &p,
            "{'descr': '<f2', 'fortran_order': False, 'shape': (3, 768), }\n",
            &vec![0u8; 3 * 768 * 2],
        );
        let h = read_npy_header(&p).unwrap();
        assert_eq!(h.dtype, DType::F16);
        assert_eq!(h.shape, vec![3, 768]);
        assert_eq!(h.data_len(), 4608);
    }

    #[test]
    fn npy_reads_v2_headers() {
        let d = dir();
        let p = d.path().join("v2.npy");
        let dict = "{'descr': '<f4', 'fortran_order': False, 'shape': (2,), }\n";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(NPY_MAGIC);
        bytes.extend_from_slice(&[2, 0]);
        bytes.extend_from_slice(&(dict.len() as u32).to_le_bytes());
        bytes.extend_from_slice(dict.as_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        fs::write(&p, bytes).unwrap();
        let h = read_npy_header(&p).unwrap();
        assert_eq!(h.shape, vec![2]);
        assert_eq!(h.data_offset, (12 + dict.len()) as u64);
    }

    #[test]
    fn npy_reads_zero_length_and_scalar_shapes() {
        let d = dir();
        let p = d.path().join("empty.npy");
        write_npy_f32(&p, &[], &[0]).unwrap();
        let h = read_npy_header(&p).unwrap();
        assert_eq!(h.shape, vec![0]);
        assert_eq!(h.data_len(), 0);

        let p2 = d.path().join("scalar.npy");
        write_raw_npy(
            &p2,
            "{'descr': '<f8', 'fortran_order': False, 'shape': (), }\n",
            &[0u8; 8],
        );
        let h2 = read_npy_header(&p2).unwrap();
        assert!(h2.shape.is_empty());
        assert_eq!(h2.element_count(), 1);
    }

    #[test]
    fn pcm_header_parses() {
        let d = dir();
        let p = d.path().join("a.pcm");
        let samples: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let total = write_pcm(&p, 2, 48000, 2, &samples).unwrap();
        assert_eq!(total, 18 + 32);
        let h = read_pcm_header(&p).unwrap();
        assert_eq!(h.version, 2);
        assert_eq!(h.sample_rate, 48000);
        assert_eq!(h.channels, 2);
        assert_eq!(h.samples, 8);
        assert_eq!(h.frames, 4);
        assert_eq!(h.data_offset, 18);
        assert_eq!(h.tensor_shape(), vec![4, 2]);
    }

    #[test]
    fn pcm_mono_shape_is_one_dimensional() {
        let d = dir();
        let p = d.path().join("m.pcm");
        write_pcm(&p, 1, 44100, 1, &[0.0, 1.0, 2.0]).unwrap();
        let h = read_pcm_header(&p).unwrap();
        assert_eq!(h.frames, 3);
        assert_eq!(h.tensor_shape(), vec![3]);
    }

    #[test]
    fn pcm_accepts_v1_and_v2_only() {
        let d = dir();
        for v in [1u32, 2] {
            let p = d.path().join(format!("v{v}.pcm"));
            write_pcm(&p, v, 48000, 1, &[0.0]).unwrap();
            assert_eq!(read_pcm_header(&p).unwrap().version, v);
        }
        let p = d.path().join("v3.pcm");
        write_pcm(&p, 3, 48000, 1, &[0.0]).unwrap();
        let e = read_pcm_header(&p).unwrap_err();
        assert!(
            e.message().contains("unsupported pcm cache version 3"),
            "{e}"
        );
    }

    #[test]
    fn pcm_rejects_short_files() {
        let d = dir();
        let p = d.path().join("short.pcm");
        fs::write(&p, [0u8; 10]).unwrap();
        let e = read_pcm_header(&p).unwrap_err();
        assert!(e.message().contains("18-byte header"), "{e}");
    }

    #[test]
    fn pcm_rejects_truncated_payload() {
        let d = dir();
        let p = d.path().join("t.pcm");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&48000u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&100u64.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        fs::write(&p, bytes).unwrap();
        let e = read_pcm_header(&p).unwrap_err();
        assert!(e.message().contains("truncated pcm file"), "{e}");
    }

    #[test]
    fn pcm_rejects_unaligned_payload() {
        let d = dir();
        let p = d.path().join("u.pcm");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&48000u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 9]); // 2 samples + one stray byte
        fs::write(&p, bytes).unwrap();
        let e = read_pcm_header(&p).unwrap_err();
        assert!(e.message().contains("whole number of f32 samples"), "{e}");
    }

    #[test]
    fn pcm_rejects_degenerate_headers() {
        let mut buf = [0u8; 18];
        buf[..4].copy_from_slice(&2u32.to_le_bytes());
        buf[4..8].copy_from_slice(&0u32.to_le_bytes());
        buf[8..10].copy_from_slice(&1u16.to_le_bytes());
        assert!(parse_pcm_header(&buf)
            .unwrap_err()
            .message()
            .contains("zero sample rate"));

        buf[4..8].copy_from_slice(&48000u32.to_le_bytes());
        buf[8..10].copy_from_slice(&0u16.to_le_bytes());
        assert!(parse_pcm_header(&buf)
            .unwrap_err()
            .message()
            .contains("zero channels"));

        buf[8..10].copy_from_slice(&2u16.to_le_bytes());
        buf[10..18].copy_from_slice(&5u64.to_le_bytes());
        assert!(parse_pcm_header(&buf)
            .unwrap_err()
            .message()
            .contains("not divisible"));

        assert!(parse_pcm_header(&[0u8; 4])
            .unwrap_err()
            .message()
            .contains("shorter than 18"));
    }

    #[test]
    fn pcm_data_starts_at_an_unaligned_offset_and_still_reads() {
        // 18 is not a multiple of 4: the reader must not assume alignment.
        let d = dir();
        let p = d.path().join("a.pcm");
        let samples = [0.5f32, -0.5, 0.25];
        write_pcm(&p, 2, 48000, 1, &samples).unwrap();
        let h = read_pcm_header(&p).unwrap();
        assert_eq!(h.data_offset % 4, 2);
        assert_eq!(raw_le::read_f32(&p, h.data_offset, 3).unwrap(), samples);
    }

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&PNG_MAGIC);
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b.extend_from_slice(&[0u8; 4]); // crc placeholder
        b
    }

    #[test]
    fn png_info_reads_dimensions_without_decoding() {
        let d = dir();
        let p = d.path().join("fig.png");
        fs::write(&p, png_bytes(1200, 400)).unwrap();
        let info = read_png_info(&p).unwrap();
        assert_eq!(info.width, 1200);
        assert_eq!(info.height, 400);
        assert_eq!(info.byte_len, fs::metadata(&p).unwrap().len());
    }

    #[test]
    fn png_rejects_non_png_and_short_files() {
        let d = dir();
        let p = d.path().join("x.png");
        fs::write(&p, b"definitely not a png file").unwrap();
        assert!(read_png_info(&p)
            .unwrap_err()
            .message()
            .contains("bad magic"));

        let p2 = d.path().join("s.png");
        fs::write(&p2, PNG_MAGIC).unwrap();
        assert!(read_png_info(&p2)
            .unwrap_err()
            .message()
            .contains("shorter than its IHDR"));

        let p3 = d.path().join("noihdr.png");
        let mut b = png_bytes(1, 1);
        b[12..16].copy_from_slice(b"iTXt");
        fs::write(&p3, b).unwrap();
        assert!(read_png_info(&p3).unwrap_err().message().contains("IHDR"));
    }
}
