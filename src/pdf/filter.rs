//! PDF stream filter decoding.

use std::borrow::Cow;

use super::deflate;
use super::syntax::decode_hex;
use super::{Dictionary, Object, PdfError, Stream};

pub(super) fn decode_filters(stream: &Stream, pdf_bytes: &[u8]) -> Result<Vec<u8>, PdfError> {
    let filters = collect_filters(&stream.dict);
    let parms = collect_parms(&stream.dict);
    let mut data = Cow::Borrowed(stream.content(pdf_bytes));
    for (i, name) in filters.iter().enumerate() {
        let dp = parms.get(i).cloned().unwrap_or_default();
        data = Cow::Owned(apply_filter(name, data.as_ref(), &dp)?);
    }
    Ok(data.into_owned())
}

pub(crate) fn collect_filters(dict: &Dictionary) -> Vec<&[u8]> {
    match dict.get(b"Filter") {
        Some(Object::Name(n)) => vec![n.as_slice()],
        Some(Object::Array(arr)) => arr.iter().filter_map(Object::as_name).collect(),
        _ => Vec::new(),
    }
}

fn collect_parms(dict: &Dictionary) -> Vec<Dictionary> {
    match dict.get(b"DecodeParms") {
        Some(Object::Dictionary(d)) => vec![d.clone()],
        Some(Object::Array(arr)) => arr
            .iter()
            .map(|o| match o {
                Object::Dictionary(d) => d.clone(),
                _ => Dictionary::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn apply_filter(name: &[u8], data: &[u8], parms: &Dictionary) -> Result<Vec<u8>, PdfError> {
    match name {
        b"FlateDecode" | b"Fl" => {
            let inflated = deflate::inflate_zlib(data)?;
            // Skipping the predictor keeps the inflated buffer, avoiding a copy.
            if predictor_enabled(parms) {
                apply_predictor(&inflated, parms)
            } else {
                Ok(inflated)
            }
        }
        b"ASCIIHexDecode" | b"AHx" => Ok(decode_hex(data)),
        b"ASCII85Decode" | b"A85" => Ok(decode_ascii85(data)),
        // Pass-through filters: the consumer reads `Stream::content`
        // directly, but if a caller invokes the chain we just hand the
        // bytes back unchanged.
        b"DCTDecode" | b"DCT" | b"JPXDecode" | b"CCITTFaxDecode" | b"CCF" => Ok(data.to_vec()),
        other => Err(PdfError::BadFilter(format!(
            "unsupported filter /{}",
            std::str::from_utf8(other).unwrap_or("?")
        ))),
    }
}

fn predictor_enabled(parms: &Dictionary) -> bool {
    parms
        .get(b"Predictor")
        .and_then(Object::as_integer)
        .unwrap_or(1)
        > 1
}

fn decode_ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 4 / 5);
    let mut buf = [0u32; 5];
    let mut n = 0;
    // Accumulate in u64: the maximum first digit alone can exceed u32::MAX.
    // Malformed groups that don't fit are skipped.
    let pack = |buf: &[u32; 5]| -> Option<u32> {
        let v = (buf[0] as u64) * 85u64.pow(4)
            + (buf[1] as u64) * 85u64.pow(3)
            + (buf[2] as u64) * 85u64.pow(2)
            + (buf[3] as u64) * 85
            + (buf[4] as u64);
        u32::try_from(v).ok()
    };
    for &b in data {
        if b == b'~' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'z' && n == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            continue;
        }
        buf[n] = (b - b'!') as u32;
        n += 1;
        if n == 5 {
            if let Some(v) = pack(&buf) {
                out.extend_from_slice(&v.to_be_bytes());
            }
            n = 0;
        }
    }
    if n > 0 {
        for slot in &mut buf[n..5] {
            *slot = 84;
        }
        if let Some(v) = pack(&buf) {
            out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
        }
    }
    out
}

fn apply_predictor(data: &[u8], parms: &Dictionary) -> Result<Vec<u8>, PdfError> {
    if !predictor_enabled(parms) {
        return Ok(data.to_vec());
    }
    // /Columns, /Colors, /BitsPerComponent are i64 in the source dict.
    // Without bounds, hostile files can choose values whose product wraps
    // in release builds or panics in debug.
    let read_clamped = |key: &[u8], default: i64, max: i64| -> Result<usize, PdfError> {
        let v = parms
            .get(key)
            .and_then(Object::as_integer)
            .unwrap_or(default);
        if v < 1 || v > max {
            return Err(PdfError::BadFilter(format!(
                "predictor /{} out of range: {v}",
                std::str::from_utf8(key).unwrap_or("?")
            )));
        }
        Ok(v as usize)
    };
    let columns = read_clamped(b"Columns", 1, 1 << 20)?;
    let colors = read_clamped(b"Colors", 1, 32)?;
    let bpc = read_clamped(b"BitsPerComponent", 8, 32)?;
    let bpp = ((colors * bpc) + 7) / 8;
    let row_len = ((columns * colors * bpc) + 7) / 8;
    let stride = row_len + 1;
    let rows = data.len() / stride;
    let mut out = Vec::with_capacity(rows * row_len);
    let mut prev_row: Vec<u8> = vec![0u8; row_len];
    for r in 0..rows {
        let row = &data[r * stride..r * stride + stride];
        let tag = row[0];
        let raw = &row[1..];
        let mut decoded = vec![0u8; row_len];
        for i in 0..row_len {
            let left = if i >= bpp { decoded[i - bpp] } else { 0 };
            let up = prev_row[i];
            let upper_left = if i >= bpp { prev_row[i - bpp] } else { 0 };
            decoded[i] = match tag {
                0 => raw[i],
                1 => raw[i].wrapping_add(left),
                2 => raw[i].wrapping_add(up),
                3 => raw[i].wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => raw[i].wrapping_add(paeth(left, up, upper_left)),
                _ => raw[i],
            };
        }
        out.extend_from_slice(&decoded);
        prev_row = decoded;
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i32 + b as i32 - c as i32;
    let pa = (p - a as i32).abs();
    let pb = (p - b as i32).abs();
    let pc = (p - c as i32).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(entries: &[(&[u8], i64)]) -> Dictionary {
        let mut d = Dictionary::new();
        for (key, value) in entries {
            d.insert(key.to_vec(), Object::Integer(*value));
        }
        d
    }

    #[test]
    fn collect_filters_handles_each_shape() {
        let mut d = Dictionary::new();
        assert!(collect_filters(&d).is_empty());
        d.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
        assert_eq!(collect_filters(&d), vec![&b"FlateDecode"[..]]);

        let arr = Object::Array(vec![
            Object::Name(b"ASCIIHexDecode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
            Object::Integer(0), // ignored — not a name
        ]);
        d.insert(b"Filter".to_vec(), arr);
        assert_eq!(
            collect_filters(&d),
            vec![&b"ASCIIHexDecode"[..], &b"FlateDecode"[..]],
        );
    }

    #[test]
    fn collect_parms_handles_each_shape() {
        let mut d = Dictionary::new();
        assert!(collect_parms(&d).is_empty());
        let sub = params(&[(b"Predictor", 12)]);
        d.insert(b"DecodeParms".to_vec(), Object::Dictionary(sub.clone()));
        assert_eq!(collect_parms(&d).len(), 1);
        d.insert(
            b"DecodeParms".to_vec(),
            Object::Array(vec![
                Object::Dictionary(sub),
                Object::Integer(0), // becomes an empty dict
            ]),
        );
        assert_eq!(collect_parms(&d).len(), 2);
    }

    #[test]
    fn decode_filters_propagates_inflate_error() {
        let mut dict = Dictionary::new();
        dict.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
        let stream = Stream::owned(dict, b"garbage".to_vec());
        assert!(decode_filters(&stream, b"").is_err());
    }

    #[test]
    fn ascii85_filter_decodes() {
        // "Hello, world!" encoded with stock Ascii85, plus a trailing `~`
        // sentinel that the decoder should treat as end-of-data.
        assert_eq!(decode_ascii85(b"87cURD_*#TDfTZ)+T~>"), b"Hello, world!");
        // `z` shortcut: four zero bytes.
        assert_eq!(decode_ascii85(b"z~>"), vec![0, 0, 0, 0]);
        // Whitespace within the encoding is ignored.
        assert_eq!(decode_ascii85(b"87cU\nRD_*#T\nDfTZ)+T~>"), b"Hello, world!");
        // Bytes outside the Ascii85 alphabet (other than whitespace, `z`,
        // and the `~` sentinel) are silently skipped. Four bytes of `!`
        // produce 3 padded output bytes (`n - 1`).
        assert_eq!(decode_ascii85(b"!\xFF!!!~>"), vec![0u8, 0, 0]);
    }

    #[test]
    fn ascii85_oversized_group_is_skipped_not_panicking() {
        // Five `u` characters decode to 84*85^4 + ... > u32::MAX. The
        // legacy u32 arithmetic panicked in debug; we now skip the group
        // and return whatever decoded successfully (nothing here).
        assert!(decode_ascii85(b"uuuuu~>").is_empty());
    }

    #[test]
    fn apply_filter_dispatch_covers_each_filter() {
        let empty = Dictionary::new();
        // FlateDecode (zlib of "hi")
        let zlib = [0x78, 0x9C, 0xCB, 0xC8, 0x04, 0x00, 0x01, 0x3D, 0x00, 0xD2];
        let a85 = b"87cURD_*#TDfTZ)+T~>";
        for (name, data, expected) in [
            (b"FlateDecode".as_slice(), &zlib[..], b"hi".as_slice()),
            (b"Fl", &zlib[..], b"hi"),
            (b"ASCIIHexDecode", b"4869>", b"Hi"),
            (b"AHx", b"4869>", b"Hi"),
            (b"ASCII85Decode", a85, b"Hello, world!"),
            (b"A85", a85, b"Hello, world!"),
            // Pass-through filters return data unchanged.
            (b"DCTDecode", b"abc", b"abc"),
            (b"DCT", b"abc", b"abc"),
            (b"JPXDecode", b"abc", b"abc"),
            (b"CCITTFaxDecode", b"abc", b"abc"),
            (b"CCF", b"abc", b"abc"),
        ] {
            assert_eq!(apply_filter(name, data, &empty).unwrap(), expected);
        }
        assert!(apply_filter(b"LZWDecode", b"abc", &empty).is_err());
    }

    #[test]
    fn flate_filter_applies_png_predictor_without_extra_copy_path() {
        let parms = params(&[(b"Predictor", 12), (b"Columns", 3)]);
        let raw = [0, 10, 20, 30];
        let mut zlib = vec![0x78, 0x01, 0x01, raw.len() as u8, 0x00];
        zlib.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(&raw);
        zlib.extend_from_slice(&[0, 0, 0, 1]);
        assert_eq!(
            apply_filter(b"FlateDecode", &zlib, &parms).unwrap(),
            [10, 20, 30]
        );
    }

    #[test]
    fn predictor_passes_through_when_disabled() {
        let parms = params(&[(b"Predictor", 1)]);
        assert_eq!(apply_predictor(b"hello", &parms).unwrap(), b"hello");
    }

    #[test]
    fn predictor_decodes_each_png_filter() {
        // 3 columns, 1 colour, 8 bpc → row length 3 → stride 4.
        // Two rows, each with a different filter tag, decode back to the
        // same data we'd have if no predictor were in use.
        let parms = params(&[
            (b"Predictor", 12),
            (b"Columns", 3),
            (b"Colors", 1),
            (b"BitsPerComponent", 8),
        ]);

        // Build raw row data, then encode each filter manually.
        // Plain rows: r0 = [10, 20, 30], r1 = [11, 22, 33]
        let r0 = [10u8, 20, 30];
        let r1 = [11u8, 22, 33];

        // tag 0 (None): row bytes pass through.
        let f0: Vec<u8> = std::iter::once(0).chain(r0.iter().copied()).collect();
        // tag 1 (Sub): subtract left
        let f1: Vec<u8> = std::iter::once(1)
            .chain([r1[0], r1[1].wrapping_sub(r1[0]), r1[2].wrapping_sub(r1[1])])
            .collect();
        let decoded = apply_predictor(&[f0, f1].concat(), &parms).unwrap();
        assert_eq!(&decoded[..3], &r0);
        assert_eq!(&decoded[3..6], &r1);

        // tag 2 (Up): up reference is row above.
        let f0v: Vec<u8> = std::iter::once(2).chain(r0.iter().copied()).collect();
        let f1v: Vec<u8> = std::iter::once(2)
            .chain(r1.iter().zip(r0.iter()).map(|(a, b)| a.wrapping_sub(*b)))
            .collect();
        let decoded = apply_predictor(&[f0v, f1v].concat(), &parms).unwrap();
        assert_eq!(&decoded[3..6], &r1);

        // tag 3 (Average) and tag 4 (Paeth) — round-trip a single zero row
        // with a known previous row so the helpers run end-to-end.
        let prev = [5u8, 10, 15];
        let next = [7u8, 12, 22];
        let prev_row: Vec<u8> = std::iter::once(0).chain(prev.iter().copied()).collect();
        let f3: Vec<u8> = std::iter::once(3)
            .chain([
                next[0].wrapping_sub((prev[0] as u16 / 2) as u8),
                next[1].wrapping_sub(((next[0] as u16 + prev[1] as u16) / 2) as u8),
                next[2].wrapping_sub(((next[1] as u16 + prev[2] as u16) / 2) as u8),
            ])
            .collect();
        let decoded = apply_predictor(&[prev_row.clone(), f3].concat(), &parms).unwrap();
        assert_eq!(&decoded[3..6], &next);

        let f4: Vec<u8> = std::iter::once(4)
            .chain([
                next[0].wrapping_sub(paeth(0, prev[0], 0u8)),
                next[1].wrapping_sub(paeth(next[0], prev[1], prev[0])),
                next[2].wrapping_sub(paeth(next[1], prev[2], prev[1])),
            ])
            .collect();
        let decoded = apply_predictor(&[prev_row, f4].concat(), &parms).unwrap();
        assert_eq!(&decoded[3..6], &next);
    }

    #[test]
    fn predictor_unknown_tag_passes_raw_bytes_through() {
        let parms = params(&[(b"Predictor", 12), (b"Columns", 2)]);
        // tag 99 hits the fallback arm.
        assert_eq!(apply_predictor(&[99u8, 1, 2], &parms).unwrap(), vec![1, 2]);
    }

    #[test]
    fn predictor_rejects_out_of_range_parameters() {
        for entry in [
            (b"Columns".as_slice(), 0),
            (b"Columns", i64::MAX),
            (b"Colors", -1),
            (b"BitsPerComponent", -1),
        ] {
            let parms = params(&[(b"Predictor", 12), entry]);
            assert!(apply_predictor(&[1, 2, 3], &parms).is_err(), "{entry:?}");
        }
    }

    #[test]
    fn paeth_predictor_picks_each_branch() {
        // Equal distances → picks `a` (first arm).
        assert_eq!(paeth(10, 10, 10), 10);
        // pa < pb && pa < pc → picks `a`.
        assert_eq!(paeth(10, 20, 30), 10);
        // pa > pb && pb <= pc → picks `b` (middle arm).
        assert_eq!(paeth(0, 5, 0), 5);
        // pb > pc → picks `c` (final arm).
        assert_eq!(paeth(8, 10, 9), 9);
    }
}
