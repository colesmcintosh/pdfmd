//! PDF stream filter decoding.

use std::borrow::Cow;

use super::deflate;
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

pub(super) fn collect_filters(dict: &Dictionary) -> Vec<Vec<u8>> {
    match dict.get(b"Filter") {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(arr)) => arr
            .iter()
            .filter_map(|o| o.as_name().map(|n| n.to_vec()))
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn collect_parms(dict: &Dictionary) -> Vec<Dictionary> {
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

pub(super) fn apply_filter(
    name: &[u8],
    data: &[u8],
    parms: &Dictionary,
) -> Result<Vec<u8>, PdfError> {
    match name {
        b"FlateDecode" | b"Fl" => {
            let inflated = deflate::inflate_zlib(data)?;
            apply_predictor_owned(inflated, parms)
        }
        b"ASCIIHexDecode" | b"AHx" => Ok(decode_ascii_hex(data)),
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

fn apply_predictor_owned(data: Vec<u8>, parms: &Dictionary) -> Result<Vec<u8>, PdfError> {
    let predictor = parms
        .get(b"Predictor")
        .and_then(Object::as_integer)
        .unwrap_or(1);
    if predictor <= 1 {
        return Ok(data);
    }
    apply_predictor(&data, parms)
}

pub(super) fn decode_ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut nibble: Option<u8> = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        match nibble {
            Some(prev) => {
                out.push((prev << 4) | v);
                nibble = None;
            }
            None => nibble = Some(v),
        }
    }
    if let Some(prev) = nibble {
        out.push(prev << 4);
    }
    out
}

pub(super) fn decode_ascii85(data: &[u8]) -> Vec<u8> {
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

pub(super) fn apply_predictor(data: &[u8], parms: &Dictionary) -> Result<Vec<u8>, PdfError> {
    let predictor = parms
        .get(b"Predictor")
        .and_then(Object::as_integer)
        .unwrap_or(1);
    if predictor <= 1 {
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

pub(super) fn paeth(a: u8, b: u8, c: u8) -> u8 {
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
