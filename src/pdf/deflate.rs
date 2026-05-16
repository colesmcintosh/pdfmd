//! From-scratch zlib (RFC 1950) / DEFLATE (RFC 1951) decoder.
//!
//! Just enough to decompress the `FlateDecode` streams that PDFs produce —
//! stored, fixed-Huffman, and dynamic-Huffman blocks. Adler-32 verification
//! is skipped; PDF consumers don't enforce it and dropping the per-byte pass
//! is measurable on the hot path.

use super::PdfError;

const MAX_BITS: u32 = 15;
const TABLE_SIZE: usize = 1 << MAX_BITS;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Decompress a zlib-wrapped DEFLATE payload.
pub fn inflate_zlib(input: &[u8]) -> Result<Vec<u8>, PdfError> {
    if input.len() < 2 {
        return Err(PdfError::Deflate("zlib stream truncated".into()));
    }
    let cmf = input[0];
    let flg = input[1];
    if cmf & 0x0F != 8 {
        return Err(PdfError::Deflate(format!(
            "unsupported zlib method {}",
            cmf & 0x0F
        )));
    }
    // FCHECK validation: (cmf * 256 + flg) must be divisible by 31.
    if ((cmf as u16) << 8 | flg as u16) % 31 != 0 {
        return Err(PdfError::Deflate("bad zlib header check".into()));
    }
    let mut start = 2;
    if flg & 0x20 != 0 {
        // FDICT: 4-byte dictionary id we don't track.
        if input.len() < 6 {
            return Err(PdfError::Deflate("zlib FDICT truncated".into()));
        }
        start = 6;
    }
    // Last 4 bytes are an adler-32 checksum we ignore.
    let end = input.len().saturating_sub(4).max(start);
    inflate_raw(&input[start..end])
}

/// Decompress a raw DEFLATE stream (no zlib wrapper).
pub fn inflate_raw(input: &[u8]) -> Result<Vec<u8>, PdfError> {
    let mut reader = BitReader::new(input);
    // Generous initial guess: most PDF streams expand 2-4x.
    let mut out = Vec::with_capacity(input.len() * 4);
    loop {
        let bfinal = reader.read(1)?;
        let btype = reader.read(2)?;
        match btype {
            0 => decode_stored(&mut reader, &mut out)?,
            1 => decode_huffman(
                &mut reader,
                &mut out,
                &fixed_ll_table(),
                &fixed_dist_table(),
            )?,
            2 => {
                let (ll, dist) = read_dynamic_tables(&mut reader)?;
                decode_huffman(&mut reader, &mut out, &ll, &dist)?;
            }
            _ => return Err(PdfError::Deflate("reserved DEFLATE block type".into())),
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok(out)
}

fn decode_stored(reader: &mut BitReader<'_>, out: &mut Vec<u8>) -> Result<(), PdfError> {
    reader.align_byte();
    let len = reader.read_aligned_u16()? as usize;
    let nlen = reader.read_aligned_u16()?;
    if (len as u16) != !nlen {
        return Err(PdfError::Deflate("stored block LEN/NLEN mismatch".into()));
    }
    out.reserve(len);
    for _ in 0..len {
        let b = reader
            .read_byte()
            .ok_or_else(|| PdfError::Deflate("stored block truncated".into()))?;
        out.push(b);
    }
    Ok(())
}

fn decode_huffman(
    reader: &mut BitReader<'_>,
    out: &mut Vec<u8>,
    ll: &HuffmanTable,
    dist: &HuffmanTable,
) -> Result<(), PdfError> {
    loop {
        let sym = ll.decode(reader)?;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let li = (sym - 257) as usize;
            if li >= LENGTH_BASE.len() {
                return Err(PdfError::Deflate(format!("bad length symbol {sym}")));
            }
            let length = LENGTH_BASE[li] as usize + reader.read(LENGTH_EXTRA[li] as u32)? as usize;
            let dsym = dist.decode(reader)? as usize;
            if dsym >= DIST_BASE.len() {
                return Err(PdfError::Deflate(format!("bad distance symbol {dsym}")));
            }
            let distance =
                DIST_BASE[dsym] as usize + reader.read(DIST_EXTRA[dsym] as u32)? as usize;
            if distance == 0 || distance > out.len() {
                return Err(PdfError::Deflate(format!(
                    "distance {distance} out of bounds (have {})",
                    out.len()
                )));
            }
            copy_match(out, length, distance);
        }
    }
}

/// LZ77 back-reference copy. Handles `length > distance` by repeating the
/// last `distance` bytes, which is how DEFLATE encodes runs.
fn copy_match(out: &mut Vec<u8>, length: usize, distance: usize) {
    let start = out.len() - distance;
    out.reserve(length);
    if distance >= length {
        // Non-overlapping: one contiguous copy from the existing range.
        let end = start + length;
        // SAFETY: split the borrow by copying into a temporary range.
        let src_ptr = out.as_ptr();
        let dst_len = out.len();
        unsafe {
            let dst = out.as_mut_ptr().add(dst_len);
            std::ptr::copy_nonoverlapping(src_ptr.add(start), dst, length);
            out.set_len(dst_len + length);
        }
        let _ = end;
    } else {
        for i in 0..length {
            let b = out[start + i];
            out.push(b);
        }
    }
}

fn read_dynamic_tables(
    reader: &mut BitReader<'_>,
) -> Result<(HuffmanTable, HuffmanTable), PdfError> {
    let hlit = reader.read(5)? as usize + 257;
    let hdist = reader.read(5)? as usize + 1;
    let hclen = reader.read(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(PdfError::Deflate("hlit/hdist out of range".into()));
    }

    let mut cl_lengths = [0u8; 19];
    for &i in &CODE_LENGTH_ORDER[..hclen] {
        cl_lengths[i] = reader.read(3)? as u8;
    }
    let cl_table = HuffmanTable::build(&cl_lengths)?;

    let total = hlit + hdist;
    let mut lengths = vec![0u8; total];
    let mut i = 0;
    while i < total {
        let sym = cl_table.decode(reader)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err(PdfError::Deflate("code-length repeat with no prior".into()));
                }
                let repeat = 3 + reader.read(2)? as usize;
                let prev = lengths[i - 1];
                for _ in 0..repeat {
                    if i >= total {
                        return Err(PdfError::Deflate("code-length overrun".into()));
                    }
                    lengths[i] = prev;
                    i += 1;
                }
            }
            17 => {
                let repeat = 3 + reader.read(3)? as usize;
                for _ in 0..repeat {
                    if i >= total {
                        return Err(PdfError::Deflate("code-length overrun".into()));
                    }
                    lengths[i] = 0;
                    i += 1;
                }
            }
            18 => {
                let repeat = 11 + reader.read(7)? as usize;
                for _ in 0..repeat {
                    if i >= total {
                        return Err(PdfError::Deflate("code-length overrun".into()));
                    }
                    lengths[i] = 0;
                    i += 1;
                }
            }
            _ => return Err(PdfError::Deflate(format!("bad code-length symbol {sym}"))),
        }
    }

    let ll = HuffmanTable::build(&lengths[..hlit])?;
    let dist = HuffmanTable::build(&lengths[hlit..])?;
    Ok((ll, dist))
}

// ---- Huffman ---------------------------------------------------------------

/// Canonical Huffman decode table.
///
/// `entries[bits]` is a packed `(symbol << 5) | length` value. We index with
/// the next `MAX_BITS` source bits; the consumer reads `length` of them and
/// keeps `symbol`. Length `0` means "no code mapped" (corrupt stream).
struct HuffmanTable {
    entries: Box<[u32; TABLE_SIZE]>,
}

impl HuffmanTable {
    fn build(lengths: &[u8]) -> Result<Self, PdfError> {
        let mut count = [0u32; (MAX_BITS + 1) as usize];
        for &l in lengths {
            if l as u32 > MAX_BITS {
                return Err(PdfError::Deflate(format!("code length {l} exceeds 15")));
            }
            if l > 0 {
                count[l as usize] += 1;
            }
        }

        // Special case: a single non-zero symbol (often a distance code of
        // length 1) — RFC 1951 leaves the second code undefined, but we map
        // both halves of the 1-bit table to that symbol.
        let total_codes: u32 = count[1..].iter().sum();
        if total_codes == 0 {
            return Ok(Self {
                entries: Box::new([0u32; TABLE_SIZE]),
            });
        }

        // Canonical-Huffman base code per length (RFC 1951 §3.2.2).
        let mut next_code = [0u32; (MAX_BITS + 2) as usize];
        let mut code = 0u32;
        for bits in 1..=MAX_BITS as usize {
            code = (code + count[bits - 1]) << 1;
            next_code[bits] = code;
        }

        let mut table = Box::new([0u32; TABLE_SIZE]);
        for (sym, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let len = len as u32;
            let canon = next_code[len as usize];
            next_code[len as usize] += 1;
            let reversed = reverse_bits(canon, len);
            let entry = ((sym as u32) << 5) | len;
            let stride = 1u32 << len;
            let mut idx = reversed;
            while (idx as usize) < TABLE_SIZE {
                table[idx as usize] = entry;
                idx += stride;
            }
        }
        Ok(Self { entries: table })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u32, PdfError> {
        let bits = reader.peek(MAX_BITS)?;
        let entry = self.entries[bits as usize];
        let len = entry & 0x1F;
        if len == 0 {
            return Err(PdfError::Deflate("invalid Huffman code".into()));
        }
        reader.consume(len);
        Ok(entry >> 5)
    }
}

fn reverse_bits(mut v: u32, bits: u32) -> u32 {
    let mut r = 0u32;
    for _ in 0..bits {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    r
}

// Built once per process — fixed Huffman trees are an RFC 1951 constant.
fn fixed_ll_table() -> HuffmanTable {
    let mut lens = [0u8; 288];
    for l in lens.iter_mut().take(144) {
        *l = 8;
    }
    for l in lens.iter_mut().take(256).skip(144) {
        *l = 9;
    }
    for l in lens.iter_mut().take(280).skip(256) {
        *l = 7;
    }
    for l in lens.iter_mut().take(288).skip(280) {
        *l = 8;
    }
    HuffmanTable::build(&lens).expect("fixed LL table is well-formed")
}

fn fixed_dist_table() -> HuffmanTable {
    HuffmanTable::build(&[5u8; 30]).expect("fixed dist table is well-formed")
}

// ---- Bit reader ------------------------------------------------------------

struct BitReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    buf: u64,
    buf_bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            buf: 0,
            buf_bits: 0,
        }
    }

    #[inline(always)]
    fn fill(&mut self) {
        while self.buf_bits <= 56 && self.pos < self.bytes.len() {
            self.buf |= (self.bytes[self.pos] as u64) << self.buf_bits;
            self.buf_bits += 8;
            self.pos += 1;
        }
    }

    fn peek(&mut self, n: u32) -> Result<u32, PdfError> {
        self.fill();
        if self.buf_bits < n {
            // Pad with zeros at EOF — final code may not need all 15 bits.
            return Ok((self.buf & ((1u64 << n) - 1)) as u32);
        }
        Ok((self.buf & ((1u64 << n) - 1)) as u32)
    }

    fn consume(&mut self, n: u32) {
        self.buf >>= n;
        self.buf_bits = self.buf_bits.saturating_sub(n);
    }

    fn read(&mut self, n: u32) -> Result<u32, PdfError> {
        let v = self.peek(n)?;
        self.consume(n);
        Ok(v)
    }

    fn align_byte(&mut self) {
        let drop = self.buf_bits & 7;
        self.consume(drop);
    }

    fn read_aligned_u16(&mut self) -> Result<u16, PdfError> {
        let lo = self
            .read_byte()
            .ok_or_else(|| PdfError::Deflate("aligned u16 truncated".into()))?;
        let hi = self
            .read_byte()
            .ok_or_else(|| PdfError::Deflate("aligned u16 truncated".into()))?;
        Ok(((hi as u16) << 8) | lo as u16)
    }

    fn read_byte(&mut self) -> Option<u8> {
        if self.buf_bits >= 8 {
            let b = (self.buf & 0xFF) as u8;
            self.consume(8);
            return Some(b);
        }
        if self.pos < self.bytes.len() {
            // align_byte() leaves us byte-aligned but buf may still hold
            // <8 bits at EOF; in practice align_byte was called first so we
            // can take from the underlying byte stream.
            let b = self.bytes[self.pos];
            self.pos += 1;
            Some(b)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_fixed_huffman_block_from_zlib() {
        // zlib(b"Hello, world!") produced by Python's zlib.compress.
        let zlib = [
            0x78, 0x9C, 0xF3, 0x48, 0xCD, 0xC9, 0xC9, 0xD7, 0x51, 0x28, 0xCF, 0x2F, 0xCA, 0x49,
            0x51, 0x04, 0x00, 0x1F, 0x9E, 0x04, 0x6A,
        ];
        let out = inflate_zlib(&zlib).unwrap();
        assert_eq!(out, b"Hello, world!");
    }

    #[test]
    fn stored_block_roundtrip() {
        // BFINAL=1, BTYPE=00, LEN=5, NLEN=~5, "ABCDE"
        // First byte: bits 0-2 = 001 (final + stored) → 0b001 = 1
        // Followed by 5 padding bits, then LEN/NLEN/data.
        let raw = [0x01, 0x05, 0x00, 0xFA, 0xFF, b'A', b'B', b'C', b'D', b'E'];
        let out = inflate_raw(&raw).unwrap();
        assert_eq!(out, b"ABCDE");
    }

    #[test]
    fn rejects_bad_method() {
        let bad = [0x79, 0x9C, 0x00];
        assert!(inflate_zlib(&bad).is_err());
    }

    #[test]
    fn fixed_huffman_run_length() {
        // zlib.compress(b'a' * 40) — emits a fixed-Huffman block with a
        // back-reference that copies the run.
        let zlib = [
            0x78, 0x9C, 0x4B, 0x4C, 0x24, 0x0E, 0x00, 0x00, 0x36, 0xEB, 0x0F, 0x29,
        ];
        let out = inflate_zlib(&zlib).unwrap();
        assert_eq!(out.len(), 40);
        assert!(out.iter().all(|&b| b == b'a'));
    }

    #[test]
    fn dynamic_huffman_roundtrip() {
        // zlib.compress(b'The quick brown fox jumps over the lazy dog. ' * 20)
        // picks a dynamic-Huffman block at that size.
        let zlib = [
            0x78, 0x9C, 0x0B, 0xC9, 0x48, 0x55, 0x28, 0x2C, 0xCD, 0x4C, 0xCE, 0x56, 0x48, 0x2A,
            0xCA, 0x2F, 0xCF, 0x53, 0x48, 0xCB, 0xAF, 0x50, 0xC8, 0x2A, 0xCD, 0x2D, 0x28, 0x56,
            0xC8, 0x2F, 0x4B, 0x2D, 0x52, 0x28, 0x01, 0x4A, 0xE7, 0x24, 0x56, 0x55, 0x2A, 0xA4,
            0xE4, 0xA7, 0xEB, 0x29, 0x84, 0x8C, 0x2A, 0x1E, 0x55, 0x3C, 0xAA, 0x98, 0xDA, 0x8A,
            0x01, 0x47, 0xA5, 0x43, 0x1C,
        ];
        let out = inflate_zlib(&zlib).unwrap();
        let expected: Vec<u8> = b"The quick brown fox jumps over the lazy dog. "
            .iter()
            .copied()
            .cycle()
            .take(900)
            .collect();
        assert_eq!(out, expected);
    }
}
