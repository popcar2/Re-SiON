//! Bounds-safe readers for the file formats' header fields: past the end of the data they read 0.

pub fn u16le(d: &[u8], o: usize) -> u16 { d.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]])) }
pub fn u32le(d: &[u8], o: usize) -> u32 { d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) }
pub fn u16be(d: &[u8], o: usize) -> u16 { d.get(o..o + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]])) }
pub fn byte(d: &[u8], o: usize) -> u8 { d.get(o).copied().unwrap_or(0) }

/// A text field of `n` bytes at `o`: up to the first NUL, trimmed.
pub fn text(d: &[u8], o: usize, n: usize) -> String {
    let b = d.get(o..(o + n).min(d.len())).unwrap_or(&[]);
    String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or(&[])).trim().to_string()
}
