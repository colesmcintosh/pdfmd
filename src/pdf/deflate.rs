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
        let bfinal = reader.read(1);
        let btype = reader.read(2);
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
            // Hlit caps the LL alphabet at 286, so `sym - 257` is always a
            // valid LENGTH_BASE index — no bounds check needed.
            let li = (sym - 257) as usize;
            let length = LENGTH_BASE[li] as usize + reader.read(LENGTH_EXTRA[li] as u32) as usize;
            // Hdist caps the distance alphabet at 30, same story.
            let dsym = dist.decode(reader)? as usize;
            let distance = DIST_BASE[dsym] as usize + reader.read(DIST_EXTRA[dsym] as u32) as usize;
            if distance > out.len() {
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
        // SAFETY: src and dst don't overlap (distance >= length), the source
        // window is in-bounds (caller already validated `distance <= out.len()`),
        // and we extend `out` by exactly `length` after the copy.
        let src_ptr = out.as_ptr();
        let dst_len = out.len();
        unsafe {
            let dst = out.as_mut_ptr().add(dst_len);
            std::ptr::copy_nonoverlapping(src_ptr.add(start), dst, length);
            out.set_len(dst_len + length);
        }
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
    let hlit = reader.read(5) as usize + 257;
    let hdist = reader.read(5) as usize + 1;
    let hclen = reader.read(4) as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(PdfError::Deflate("hlit/hdist out of range".into()));
    }

    let mut cl_lengths = [0u8; 19];
    for &i in &CODE_LENGTH_ORDER[..hclen] {
        cl_lengths[i] = reader.read(3) as u8;
    }
    // Each cl length is read as 3 bits → value 0..=7, well under the
    // 15-bit cap that build rejects on.
    let cl_table = HuffmanTable::build(&cl_lengths).expect("cl lengths fit");

    let total = hlit + hdist;
    let mut lengths = vec![0u8; total];
    let mut i = 0;
    while i < total {
        // The code-length tree has 19 symbols (0..=18), so `decode` never
        // returns a value outside that range — no `_` fallthrough needed.
        let sym = cl_table.decode(reader)?;
        if sym <= 15 {
            lengths[i] = sym as u8;
            i += 1;
        } else {
            // Opcodes 16/17/18 expand into repeats of a previous (or zero)
            // length. Clamp the count to what's left in the array so a
            // malformed producer can't push us past `total` — that lets us
            // keep the loop infallible.
            let (count, value) = match sym {
                16 => {
                    let n = 3 + reader.read(2) as usize;
                    let prev = if i == 0 { 0 } else { lengths[i - 1] };
                    (n, prev)
                }
                17 => (3 + reader.read(3) as usize, 0),
                // sym == 18 — the last symbol the cl table emits.
                _ => (11 + reader.read(7) as usize, 0),
            };
            let count = count.min(total - i);
            lengths[i..i + count].fill(value);
            i += count;
        }
    }

    // Lengths only ever holds values 0..=15: the cl-table decoder either
    // emits 0..15 directly or fills with 0 via the 17/18 repeat opcodes,
    // and the 16 opcode copies a previous value (already in range). So
    // build() never rejects here.
    let ll = HuffmanTable::build(&lengths[..hlit]).expect("ll lengths fit");
    let dist = HuffmanTable::build(&lengths[hlit..]).expect("dist lengths fit");
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

        // No live codes — return an empty table. Decode will report
        // "invalid Huffman code" on the first attempted read.
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
        let bits = reader.peek(MAX_BITS);
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

    /// Read N bits without advancing. Past EOF the result is zero-padded —
    /// callers handle the resulting "invalid code" via `HuffmanTable::decode`.
    fn peek(&mut self, n: u32) -> u32 {
        self.fill();
        (self.buf & ((1u64 << n) - 1)) as u32
    }

    fn consume(&mut self, n: u32) {
        self.buf >>= n;
        self.buf_bits = self.buf_bits.saturating_sub(n);
    }

    fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.consume(n);
        v
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
    fn rejects_truncated_zlib() {
        assert!(inflate_zlib(&[]).is_err());
        assert!(inflate_zlib(&[0x78]).is_err());
    }

    #[test]
    fn rejects_bad_fcheck() {
        // Method=8 (valid), but FCHECK doesn't make (cmf*256+flg) divisible by 31.
        let bad = [0x78, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01];
        assert!(inflate_zlib(&bad).is_err());
    }

    #[test]
    fn fdict_header_consumes_extra_four_bytes() {
        // FDICT set (bit 5 of flg = 0x20) on top of a valid zlib stream
        // would still need a valid header check. We just verify the
        // truncated-FDICT path errors when the four dictionary bytes are
        // absent.
        let bad = [0x78, 0xBB, 0x00, 0x00, 0x00]; // header + 3 bytes < 4 required
        assert!(inflate_zlib(&bad).is_err());
    }

    #[test]
    fn fdict_header_with_enough_bytes_advances_past_dict() {
        // Header (2) + dict id (4) + minimum DEFLATE (1 empty stored block,
        // 5 bytes) + adler32 (4) = 16 bytes. The empty stored block has
        // LEN=0/NLEN=0xFFFF.
        let mut buf = vec![0x78u8, 0xBB, 0xDE, 0xAD, 0xBE, 0xEF];
        buf.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]); // final stored, LEN=0
        buf.extend_from_slice(&[0, 0, 0, 1]); // adler32 (ignored)
        let out = inflate_zlib(&buf).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn reserved_block_type_errors() {
        // BFINAL=1, BTYPE=3 (reserved) — packed as 0b111 in the first byte.
        let bad = [0x07];
        assert!(inflate_raw(&bad).is_err());
    }

    #[test]
    fn stored_block_with_bad_nlen_errors() {
        // BFINAL=1, BTYPE=00, then LEN=5, NLEN=0 (should be ~5 = 0xFFFA).
        let bad = [0x01, 0x05, 0x00, 0x00, 0x00, b'A', b'B', b'C', b'D', b'E'];
        assert!(inflate_raw(&bad).is_err());
    }

    #[test]
    fn stored_block_truncated_errors() {
        // LEN=5 but only 2 body bytes follow.
        let bad = [0x01, 0x05, 0x00, 0xFA, 0xFF, b'A', b'B'];
        assert!(inflate_raw(&bad).is_err());
    }

    #[test]
    fn aligned_u16_truncation_propagates() {
        // BFINAL=1, BTYPE=00, then only one byte of the 2-byte LEN before EOF.
        let bad = [0x01, 0x00];
        assert!(inflate_raw(&bad).is_err());
        // Truncation between LEN and NLEN.
        let bad = [0x01, 0x00, 0x00];
        assert!(inflate_raw(&bad).is_err());
    }

    #[test]
    fn build_rejects_length_above_15() {
        let mut lens = [0u8; 5];
        lens[0] = 16;
        assert!(HuffmanTable::build(&lens).is_err());
    }

    #[test]
    fn empty_huffman_table_decodes_to_invalid_code() {
        let table = HuffmanTable::build(&[0u8; 5]).unwrap();
        let mut reader = BitReader::new(&[0xFFu8, 0xFF]);
        assert!(table.decode(&mut reader).is_err());
    }

    #[test]
    fn reverse_bits_helper() {
        assert_eq!(reverse_bits(0b101, 3), 0b101);
        assert_eq!(reverse_bits(0b1100, 4), 0b0011);
        assert_eq!(reverse_bits(0, 0), 0);
    }

    #[test]
    fn bit_reader_byte_helpers() {
        let mut r = BitReader::new(&[0xAB, 0xCD]);
        assert_eq!(r.read(4), 0xB);
        r.align_byte();
        assert_eq!(r.read_byte(), Some(0xCD));
        assert_eq!(r.read_byte(), None);
    }

    #[test]
    fn bit_reader_returns_zero_padded_bits_past_eof() {
        let mut r = BitReader::new(&[]);
        assert_eq!(r.peek(5), 0);
    }

    #[test]
    fn copy_match_handles_run_length_overlap() {
        let mut out = vec![b'X'];
        copy_match(&mut out, 5, 1);
        assert_eq!(out, b"XXXXXX");
    }

    #[test]
    fn copy_match_uses_fast_path_for_non_overlap() {
        let mut out = b"hello".to_vec();
        copy_match(&mut out, 3, 5);
        assert_eq!(out, b"hellohel");
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
    fn fixed_huffman_back_reference_with_out_of_bounds_distance_errors() {
        // Hand-built fixed-Huffman block: BFINAL=1, BTYPE=01, then a length
        // code 257 (canon 1, 7 bits → reversed = 0b1000000), then distance
        // code 0 (5 bits, all zero). Distance is 1, output is empty → fails.
        // Byte 0: bit0=1 (BFINAL), bit1=1 (BTYPE LSB), bit2=0 (BTYPE MSB),
        //         bits 3-7: low 5 bits of the reversed length-code index = 0
        //         → byte 0 = 0b00000011 = 0x03.
        // Byte 1: bit 8 = 0, bit 9 = 1 (high bit of length-code index),
        //         bits 10-14 = 0 (distance code 0), bit 15 = 0
        //         → byte 1 = 0b00000010 = 0x02.
        let bad = [0x03u8, 0x02];
        let err = inflate_raw(&bad).unwrap_err();
        assert!(err.to_string().contains("distance"));
    }

    #[test]
    fn dynamic_huffman_rejects_excessive_hlit() {
        // BFINAL=1, BTYPE=10 (dynamic), HLIT=31 (so total = 31+257=288 > 286).
        // Bit stream LSB-first: BFINAL=1, BTYPE=10, HLIT=11111
        //   → first 8 bits = 1 0 1 1 1 1 1 1 → 0xFD.
        let bad = [0xFDu8, 0xFF];
        assert!(inflate_raw(&bad).is_err());
    }

    #[test]
    fn dynamic_huffman_repeat_at_index_zero_errors() {
        // BFINAL=1, BTYPE=10, hlit=0, hdist=0, hclen=0 → cl_lengths all
        // zero → build_table succeeds with an empty table → decode errors
        // with "invalid Huffman code", which we surface as Err.
        let bad = [0x05u8, 0x00, 0x00, 0x00, 0x00];
        assert!(inflate_raw(&bad).is_err());
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
