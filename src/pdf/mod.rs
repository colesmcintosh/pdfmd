//! From-scratch PDF reader.
//!
//! Covers what the text extractor needs and nothing more: classic xref
//! tables, xref streams, object streams (PDF 1.5+), the `FlateDecode`
//! filter (with optional PNG predictor), and an in-memory cache keyed by
//! object id. Encryption and incremental updates beyond a single `/Prev`
//! chain are out of scope.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

mod deflate;
mod object;
mod parser;

pub use object::{Dictionary, Object, ObjectId, Stream};

use parser::Parser;

// ---- Errors ----------------------------------------------------------------

#[derive(Debug)]
pub enum PdfError {
    NotPdf,
    BadXref(String),
    BadObject(String),
    BadFilter(String),
    Deflate(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::NotPdf => f.write_str("input does not look like a PDF"),
            PdfError::BadXref(m) => write!(f, "xref: {m}"),
            PdfError::BadObject(m) => write!(f, "object: {m}"),
            PdfError::BadFilter(m) => write!(f, "filter: {m}"),
            PdfError::Deflate(m) => write!(f, "deflate: {m}"),
        }
    }
}

impl std::error::Error for PdfError {}

// ---- Document --------------------------------------------------------------

/// Where an object actually lives. Classic xref entries are `Uncompressed`;
/// PDF 1.5+ object streams produce `Compressed` entries instead.
#[derive(Debug, Clone, Copy)]
enum XrefEntry {
    Free,
    Uncompressed { offset: u64 },
    Compressed { stream_obj: u32, index: u32 },
}

pub struct Document {
    objects: HashMap<ObjectId, Object>,
    pages: Vec<ObjectId>,
}

impl Document {
    /// Parse the entire PDF byte slice and resolve every live indirect
    /// object. Returns a self-contained `Document` that can be shared by
    /// reference across threads.
    pub fn load(bytes: &[u8]) -> Result<Self, PdfError> {
        if !bytes.starts_with(b"%PDF-") {
            return Err(PdfError::NotPdf);
        }
        let startxref = find_startxref(bytes)?;
        let (xref, trailer) = read_xref_chain(bytes, startxref)?;

        // Materialize every uncompressed object first; object streams need
        // the surrounding objects to already exist when we expand them.
        let mut objects: HashMap<ObjectId, Object> = HashMap::with_capacity(xref.len());
        let mut compressed: Vec<(ObjectId, u32, u32)> = Vec::new();
        for (id, entry) in &xref {
            match *entry {
                XrefEntry::Free => {}
                XrefEntry::Uncompressed { offset } => {
                    if let Some(obj) = parse_at(bytes, offset as usize, *id) {
                        objects.insert(*id, obj);
                    }
                }
                XrefEntry::Compressed { stream_obj, index } => {
                    compressed.push((*id, stream_obj, index));
                }
            }
        }

        // Expand each object stream once, then pull every referenced index
        // out of it. PDF 1.5+ stores most metadata objects this way.
        let mut objstm_cache: HashMap<u32, Vec<(u32, Vec<u8>)>> = HashMap::new();
        for (id, stream_obj, index) in &compressed {
            let entries = match objstm_cache.get(stream_obj) {
                Some(v) => v,
                None => {
                    let stream_id = ObjectId(*stream_obj, 0);
                    let Some(Object::Stream(s)) = objects.get(&stream_id) else {
                        continue;
                    };
                    let decoded = decode_filters(s)?;
                    let entries = parse_object_stream(&s.dict, &decoded)?;
                    objstm_cache.insert(*stream_obj, entries);
                    &objstm_cache[stream_obj]
                }
            };
            if let Some((_, bytes_)) = entries.iter().find(|(n, _)| *n == id.0) {
                if let Ok(obj) = Parser::new(bytes_).parse_object() {
                    objects.insert(*id, obj);
                }
            }
            // Index-based lookup variant — some producers don't number entries.
            if !objects.contains_key(id) {
                if let Some((_, bytes_)) = entries.get(*index as usize) {
                    if let Ok(obj) = Parser::new(bytes_).parse_object() {
                        objects.insert(*id, obj);
                    }
                }
            }
        }

        let pages = collect_pages(&objects, &trailer)?;

        Ok(Document { objects, pages })
    }

    pub fn get_object(&self, id: ObjectId) -> Option<&Object> {
        self.objects.get(&id)
    }

    /// Follow a chain of indirect references and return the terminal object.
    pub fn deref<'a>(&'a self, obj: &'a Object) -> &'a Object {
        let mut current = obj;
        for _ in 0..32 {
            match current {
                Object::Reference(id) => match self.objects.get(id) {
                    Some(o) => current = o,
                    None => return current,
                },
                _ => return current,
            }
        }
        current
    }

    /// Pages in document order.
    pub fn pages(&self) -> &[ObjectId] {
        &self.pages
    }

    /// Concatenated, filter-decoded bytes of the page's content stream(s).
    pub fn get_page_content(&self, page_id: ObjectId) -> Option<Vec<u8>> {
        let page = self.get_object(page_id)?.as_dict()?;
        let contents = page.get(b"Contents")?;
        match self.deref(contents) {
            Object::Stream(s) => decode_filters(s).ok(),
            Object::Array(items) => {
                let mut out = Vec::new();
                for item in items {
                    if let Object::Stream(s) = self.deref(item) {
                        if let Ok(mut bytes) = decode_filters(s) {
                            // PDF content streams may abut without trailing
                            // whitespace; the spec wants us to insert one.
                            if !out.is_empty()
                                && !out.last().is_some_and(|b: &u8| b.is_ascii_whitespace())
                            {
                                out.push(b'\n');
                            }
                            out.append(&mut bytes);
                        }
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Decode the stream's `/Filter` chain and return the resulting bytes.
    pub fn decode_stream(&self, stream: &Stream) -> Result<Vec<u8>, PdfError> {
        decode_filters(stream)
    }
}

// ---- Page tree walk --------------------------------------------------------

fn collect_pages(
    objects: &HashMap<ObjectId, Object>,
    trailer: &Dictionary,
) -> Result<Vec<ObjectId>, PdfError> {
    let root_ref = trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .ok_or_else(|| PdfError::BadObject("trailer /Root missing".into()))?;
    let catalog = objects
        .get(&root_ref)
        .and_then(Object::as_dict)
        .ok_or_else(|| PdfError::BadObject("catalog missing".into()))?;
    let pages_ref = catalog
        .get(b"Pages")
        .and_then(Object::as_reference)
        .ok_or_else(|| PdfError::BadObject("/Pages missing".into()))?;

    let mut out = Vec::new();
    walk_pages(objects, pages_ref, &mut out, 0)?;
    Ok(out)
}

fn walk_pages(
    objects: &HashMap<ObjectId, Object>,
    node_id: ObjectId,
    out: &mut Vec<ObjectId>,
    depth: u32,
) -> Result<(), PdfError> {
    if depth > 64 {
        return Err(PdfError::BadObject("page tree too deep".into()));
    }
    let Some(node) = objects.get(&node_id).and_then(Object::as_dict) else {
        return Ok(());
    };
    let type_ = node.get(b"Type").and_then(Object::as_name);
    if type_ == Some(b"Page".as_slice()) {
        out.push(node_id);
        return Ok(());
    }
    let Some(kids) = node.get(b"Kids").and_then(Object::as_array) else {
        return Ok(());
    };
    for kid in kids {
        if let Some(kid_id) = kid.as_reference() {
            // Decide leaf vs interior by the kid's own /Type so we tolerate
            // producers that omit /Type on the root /Pages node.
            let kid_dict = objects.get(&kid_id).and_then(Object::as_dict);
            let kt = kid_dict
                .and_then(|d| d.get(b"Type"))
                .and_then(Object::as_name);
            if kt == Some(b"Page".as_slice()) {
                out.push(kid_id);
            } else {
                walk_pages(objects, kid_id, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

// ---- Xref reading ----------------------------------------------------------

fn find_startxref(bytes: &[u8]) -> Result<u64, PdfError> {
    // Spec: `startxref` then offset then `%%EOF`, within the last 1024 bytes.
    let tail_start = bytes.len().saturating_sub(2048);
    let tail = &bytes[tail_start..];
    let needle = b"startxref";
    let pos = (0..tail.len().saturating_sub(needle.len()))
        .rev()
        .find(|&i| &tail[i..i + needle.len()] == needle)
        .ok_or_else(|| PdfError::BadXref("missing startxref".into()))?;
    let mut i = pos + needle.len();
    while i < tail.len() && tail[i].is_ascii_whitespace() {
        i += 1;
    }
    let n_start = i;
    while i < tail.len() && tail[i].is_ascii_digit() {
        i += 1;
    }
    // The slice is digit-only by construction, so utf-8 always holds. Parse
    // failures only happen on integer overflow — a ~10^19 offset PDF.
    let s = std::str::from_utf8(&tail[n_start..i]).expect("digit slice is utf-8");
    s.parse::<u64>()
        .map_err(|_| PdfError::BadXref("startxref not numeric".into()))
}

fn read_xref_chain(
    bytes: &[u8],
    startxref: u64,
) -> Result<(BTreeMap<ObjectId, XrefEntry>, Dictionary), PdfError> {
    let mut entries: BTreeMap<ObjectId, XrefEntry> = BTreeMap::new();
    let mut final_trailer: Option<Dictionary> = None;
    let mut visited: HashMap<u64, ()> = HashMap::new();
    let mut current = startxref;
    loop {
        if visited.contains_key(&current) {
            break;
        }
        visited.insert(current, ());

        let trailer = if at_keyword(bytes, current as usize, b"xref") {
            read_classic_xref(bytes, current as usize, &mut entries)?
        } else {
            read_xref_stream(bytes, current as usize, &mut entries)?
        };

        // Earliest occurrence wins for incremental updates: the entry from
        // the most-recent xref is already in the map by the time we follow
        // /Prev, so we just don't overwrite it.
        let prev = trailer
            .get(b"Prev")
            .and_then(Object::as_integer)
            .filter(|n| *n > 0);

        if final_trailer.is_none() {
            final_trailer = Some(trailer);
        }
        match prev {
            Some(p) => current = p as u64,
            None => break,
        }
    }
    // The loop always runs at least once (visited is empty on entry) and
    // sets final_trailer before any subsequent iteration. If the first
    // xref read errors we've already returned via `?`.
    Ok((
        entries,
        final_trailer.expect("first iteration sets trailer"),
    ))
}

fn at_keyword(bytes: &[u8], at: usize, kw: &[u8]) -> bool {
    bytes.get(at..at + kw.len()).is_some_and(|w| w == kw)
}

fn read_classic_xref(
    bytes: &[u8],
    at: usize,
    out: &mut BTreeMap<ObjectId, XrefEntry>,
) -> Result<Dictionary, PdfError> {
    let mut p = Parser::with_pos(bytes, at + b"xref".len());
    p.skip_ws_and_comments();
    loop {
        // Section header: `first count`. The `trailer` keyword ends it.
        let pos = p.pos;
        if at_keyword(bytes, pos, b"trailer") {
            p.pos += b"trailer".len();
            break;
        }
        let first = read_uint(bytes, &mut p.pos)?;
        skip_inline(bytes, &mut p.pos);
        let count = read_uint(bytes, &mut p.pos)?;
        skip_eol(bytes, &mut p.pos);
        for i in 0..count {
            // Each entry is 20 bytes exactly per the spec.
            if p.pos + 20 > bytes.len() {
                return Err(PdfError::BadXref("xref entry truncated".into()));
            }
            let row = &bytes[p.pos..p.pos + 20];
            p.pos += 20;
            // Spec mandates 10 ASCII digits + space + 5 ASCII digits, both
            // always valid utf-8 — non-utf8 indicates a malformed PDF that
            // we'd reject elsewhere too.
            let offset_s = std::str::from_utf8(&row[0..10]).expect("ascii digits");
            let gen_s = std::str::from_utf8(&row[11..16]).expect("ascii digits");
            let kind = row[17];
            let n = first + i;
            let g: u16 = gen_s.trim().parse().unwrap_or(0);
            let id = ObjectId(n, g);
            if out.contains_key(&id) {
                continue;
            }
            match kind {
                b'n' => {
                    let offset: u64 = offset_s.trim().parse().unwrap_or(0);
                    out.insert(id, XrefEntry::Uncompressed { offset });
                }
                b'f' => {
                    out.insert(id, XrefEntry::Free);
                }
                _ => {}
            }
        }
        p.skip_ws_and_comments();
    }
    p.skip_ws_and_comments();
    let trailer = p.parse_object()?;
    match trailer {
        Object::Dictionary(d) => Ok(d),
        _ => Err(PdfError::BadXref("trailer is not a dictionary".into())),
    }
}

fn read_xref_stream(
    bytes: &[u8],
    at: usize,
    out: &mut BTreeMap<ObjectId, XrefEntry>,
) -> Result<Dictionary, PdfError> {
    // The cross-reference stream object lives at `at`; its first line is
    // `N G obj`, same as any indirect object.
    let mut p = Parser::with_pos(bytes, at);
    let (_, obj) = p.parse_indirect_object()?;
    let Object::Stream(stream) = obj else {
        return Err(PdfError::BadXref("xref stream wasn't a stream".into()));
    };
    let dict = stream.dict.clone();
    let payload = decode_filters(&stream)?;

    let widths: Vec<usize> = dict
        .get(b"W")
        .and_then(Object::as_array)
        .ok_or_else(|| PdfError::BadXref("xref stream missing /W".into()))?
        .iter()
        .map(|o| o.as_integer().unwrap_or(0).max(0) as usize)
        .collect();
    if widths.len() != 3 {
        return Err(PdfError::BadXref(format!(
            "xref stream /W must have 3 entries, got {}",
            widths.len()
        )));
    }
    let row = widths.iter().sum::<usize>();
    if row == 0 {
        return Err(PdfError::BadXref("xref stream row width is zero".into()));
    }

    let size =
        dict.get(b"Size")
            .and_then(Object::as_integer)
            .ok_or_else(|| PdfError::BadXref("xref stream missing /Size".into()))? as u32;
    let index: Vec<u32> = match dict.get(b"Index").and_then(Object::as_array) {
        Some(arr) => arr
            .iter()
            .map(|o| o.as_integer().unwrap_or(0).max(0) as u32)
            .collect(),
        None => vec![0, size],
    };

    let mut cursor = 0usize;
    for chunk in index.chunks(2) {
        if chunk.len() < 2 {
            break;
        }
        let first = chunk[0];
        let count = chunk[1];
        for i in 0..count {
            if cursor + row > payload.len() {
                return Err(PdfError::BadXref("xref stream truncated".into()));
            }
            let row_bytes = &payload[cursor..cursor + row];
            cursor += row;
            let t = if widths[0] == 0 {
                1 // default per spec
            } else {
                be_uint(&row_bytes[..widths[0]])
            };
            let f1 = be_uint(&row_bytes[widths[0]..widths[0] + widths[1]]);
            let f2 = be_uint(&row_bytes[widths[0] + widths[1]..]);
            let id = ObjectId(first + i, 0);
            if out.contains_key(&id) {
                continue;
            }
            match t {
                0 => {
                    out.insert(id, XrefEntry::Free);
                }
                1 => {
                    out.insert(id, XrefEntry::Uncompressed { offset: f1 });
                }
                2 => {
                    out.insert(
                        id,
                        XrefEntry::Compressed {
                            stream_obj: f1 as u32,
                            index: f2 as u32,
                        },
                    );
                }
                _ => {}
            }
        }
    }
    Ok(dict)
}

fn be_uint(bytes: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for &b in bytes {
        v = (v << 8) | b as u64;
    }
    v
}

// ---- Object streams (PDF 1.5+) ---------------------------------------------

fn parse_object_stream(dict: &Dictionary, decoded: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, PdfError> {
    let n_raw = dict
        .get(b"N")
        .and_then(Object::as_integer)
        .ok_or_else(|| PdfError::BadObject("objstm missing /N".into()))?;
    let first_raw = dict
        .get(b"First")
        .and_then(Object::as_integer)
        .ok_or_else(|| PdfError::BadObject("objstm missing /First".into()))?;
    // /N is an entry count; a negative or absurd value cast through
    // `as usize` would otherwise become ~1.8×10^19 and abort the allocator
    // on the with_capacity below. Each header is at least two bytes
    // ("0 0"), so cap by `decoded.len() / 2`.
    if n_raw < 0 || (n_raw as u64) > decoded.len() as u64 / 2 {
        return Err(PdfError::BadObject(format!(
            "objstm /N out of range: {n_raw}"
        )));
    }
    if first_raw < 0 || (first_raw as u64) > decoded.len() as u64 {
        return Err(PdfError::BadObject(format!(
            "objstm /First out of range: {first_raw}"
        )));
    }
    let n = n_raw as usize;
    let first = first_raw as usize;

    // Header: N pairs of "obj_num offset" pointing into the body at byte
    // /First. The Nth object ends at the next offset (or end of stream).
    let mut p = Parser::with_pos(decoded, 0);
    let mut headers: Vec<(u32, usize)> = Vec::with_capacity(n);
    for _ in 0..n {
        p.skip_ws_and_comments();
        let num = read_uint(decoded, &mut p.pos)?;
        p.skip_ws_and_comments();
        let off = read_uint(decoded, &mut p.pos)? as usize;
        headers.push((num, off));
    }
    let mut out: Vec<(u32, Vec<u8>)> = Vec::with_capacity(n);
    for (i, &(num, off)) in headers.iter().enumerate() {
        let start = first + off;
        let end = headers
            .get(i + 1)
            .map(|(_, next_off)| first + *next_off)
            .unwrap_or(decoded.len());
        if start <= end && end <= decoded.len() {
            out.push((num, decoded[start..end].to_vec()));
        }
    }
    Ok(out)
}

// ---- Filter chain ----------------------------------------------------------

fn decode_filters(stream: &Stream) -> Result<Vec<u8>, PdfError> {
    let filters = collect_filters(&stream.dict);
    let parms = collect_parms(&stream.dict);
    let mut data = stream.content.clone();
    for (i, name) in filters.iter().enumerate() {
        let dp = parms.get(i).cloned().unwrap_or_default();
        data = apply_filter(name, &data, &dp)?;
    }
    Ok(data)
}

fn collect_filters(dict: &Dictionary) -> Vec<Vec<u8>> {
    match dict.get(b"Filter") {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(arr)) => arr
            .iter()
            .filter_map(|o| o.as_name().map(|n| n.to_vec()))
            .collect(),
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
            apply_predictor(&inflated, parms)
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

fn decode_ascii_hex(data: &[u8]) -> Vec<u8> {
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

fn decode_ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 4 / 5);
    let mut buf = [0u32; 5];
    let mut n = 0;
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
            let v = buf[0] * 85u32.pow(4)
                + buf[1] * 85u32.pow(3)
                + buf[2] * 85u32.pow(2)
                + buf[3] * 85
                + buf[4];
            out.extend_from_slice(&v.to_be_bytes());
            n = 0;
        }
    }
    if n > 0 {
        for slot in &mut buf[n..5] {
            *slot = 84;
        }
        let v = buf[0] * 85u32.pow(4)
            + buf[1] * 85u32.pow(3)
            + buf[2] * 85u32.pow(2)
            + buf[3] * 85
            + buf[4];
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    out
}

// ---- PNG predictor (used by xref streams and some image streams) -----------

fn apply_predictor(data: &[u8], parms: &Dictionary) -> Result<Vec<u8>, PdfError> {
    let predictor = parms
        .get(b"Predictor")
        .and_then(Object::as_integer)
        .unwrap_or(1);
    if predictor <= 1 {
        return Ok(data.to_vec());
    }
    let columns = parms
        .get(b"Columns")
        .and_then(Object::as_integer)
        .unwrap_or(1) as usize;
    let colors = parms
        .get(b"Colors")
        .and_then(Object::as_integer)
        .unwrap_or(1) as usize;
    let bpc = parms
        .get(b"BitsPerComponent")
        .and_then(Object::as_integer)
        .unwrap_or(8) as usize;
    let bpp = ((colors * bpc) + 7) / 8;
    let row_len = ((columns * colors * bpc) + 7) / 8;
    if row_len == 0 {
        return Ok(Vec::new());
    }
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
    let a = a as i32;
    let b = b as i32;
    let c = c as i32;
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

// ---- Helpers ---------------------------------------------------------------

fn parse_at(bytes: &[u8], at: usize, expected: ObjectId) -> Option<Object> {
    let mut p = Parser::with_pos(bytes, at);
    let (id, obj) = p.parse_indirect_object().ok()?;
    if id.0 != expected.0 {
        return None;
    }
    Some(obj)
}

fn read_uint(bytes: &[u8], pos: &mut usize) -> Result<u32, PdfError> {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\r' | b'\n') {
        *pos += 1;
    }
    let start = *pos;
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == start {
        return Err(PdfError::BadXref(format!("expected integer at {start}")));
    }
    // The slice is ASCII digits by construction — utf-8 always holds.
    let s = std::str::from_utf8(&bytes[start..*pos]).expect("digit slice is utf-8");
    s.parse::<u32>()
        .map_err(|_| PdfError::BadXref(format!("integer overflow: {s}")))
}

fn skip_inline(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t') {
        *pos += 1;
    }
}

fn skip_eol(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t') {
        *pos += 1;
    }
    match bytes.get(*pos) {
        Some(&b'\r') => {
            *pos += 1;
            if bytes.get(*pos) == Some(&b'\n') {
                *pos += 1;
            }
        }
        Some(&b'\n') => *pos += 1,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid classic-xref PDF with one `(Hello) Tj`-style
    /// content stream. Used to exercise the loader end-to-end without
    /// depending on a real PDF fixture.
    fn minimal_pdf() -> Vec<u8> {
        let body = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 612 792]/Contents 4 0 R>> endobj
4 0 obj <</Length 24>>
stream
BT /F1 12 Tf (Hi) Tj ET
endstream
endobj
";
        let mut out = body.to_vec();
        // Build a classic xref pointing at each object.
        let xref_offset = out.len();
        // Look up each obj's byte offset in the body.
        let offsets: Vec<usize> = (1..=4)
            .map(|n| {
                let needle = format!("{n} 0 obj");
                find_subslice(&out, needle.as_bytes()).unwrap()
            })
            .collect();
        let mut xref = String::from("xref\n0 5\n");
        xref.push_str("0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{:010} 00000 n \n", off));
        }
        xref.push_str("trailer <</Size 5/Root 1 0 R>>\nstartxref\n");
        xref.push_str(&format!("{xref_offset}\n"));
        xref.push_str("%%EOF\n");
        out.extend_from_slice(xref.as_bytes());
        out
    }

    fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
        (0..=hay.len().saturating_sub(needle.len())).find(|&i| &hay[i..i + needle.len()] == needle)
    }

    #[test]
    fn loads_minimal_pdf_and_walks_pages() {
        let bytes = minimal_pdf();
        let doc = Document::load(&bytes).expect("load");
        assert_eq!(doc.pages().len(), 1);
        let page = doc.pages()[0];
        let content = doc.get_page_content(page).expect("page content");
        assert!(content.windows(2).any(|w| w == b"Hi"));
    }

    #[test]
    fn rejects_non_pdf_header() {
        // Comparing on the Display string sidesteps a `match` whose unused
        // arms would otherwise show up as uncovered branches.
        let err = Document::load(b"\x00not a pdf")
            .err()
            .expect("expected Err for non-PDF input");
        assert_eq!(err.to_string(), "input does not look like a PDF");
    }

    #[test]
    fn pdf_error_display_lines() {
        // Exercise every match arm in the Display impl.
        let cases: Vec<(PdfError, &str)> = vec![
            (PdfError::NotPdf, "does not look like a PDF"),
            (PdfError::BadXref("x".into()), "xref: x"),
            (PdfError::BadObject("o".into()), "object: o"),
            (PdfError::BadFilter("f".into()), "filter: f"),
            (PdfError::Deflate("d".into()), "deflate: d"),
        ];
        for (err, expected) in cases {
            let s = format!("{err}");
            assert!(s.contains(expected), "{s} did not contain {expected}");
            // Also touch the Debug impl so it isn't dead-coded.
            let _ = format!("{err:?}");
        }
        // std::error::Error trait should be implemented.
        let _: Box<dyn std::error::Error> = Box::new(PdfError::NotPdf);
    }

    #[test]
    fn find_startxref_locates_offset() {
        let bytes = b"trash\nstartxref\n1234\n%%EOF";
        assert_eq!(find_startxref(bytes).unwrap(), 1234);
    }

    #[test]
    fn find_startxref_errors_when_missing() {
        let bytes = b"no marker at all";
        assert!(find_startxref(bytes).is_err());
    }

    #[test]
    fn find_startxref_errors_on_non_numeric_offset() {
        let bytes = b"startxref\n\n%%EOF";
        assert!(find_startxref(bytes).is_err());
    }

    // ---- Filters --------------------------------------------------------

    #[test]
    fn collect_filters_handles_each_shape() {
        let mut d = Dictionary::new();
        assert!(collect_filters(&d).is_empty());
        d.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
        assert_eq!(collect_filters(&d), vec![b"FlateDecode".to_vec()]);

        let arr = Object::Array(vec![
            Object::Name(b"ASCIIHexDecode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
            Object::Integer(0), // ignored — not a name
        ]);
        d.insert(b"Filter".to_vec(), arr);
        assert_eq!(
            collect_filters(&d),
            vec![b"ASCIIHexDecode".to_vec(), b"FlateDecode".to_vec()],
        );
    }

    #[test]
    fn collect_parms_handles_each_shape() {
        let mut d = Dictionary::new();
        assert!(collect_parms(&d).is_empty());
        let mut sub = Dictionary::new();
        sub.insert(b"Predictor".to_vec(), Object::Integer(12));
        d.insert(b"DecodeParms".to_vec(), Object::Dictionary(sub.clone()));
        assert_eq!(collect_parms(&d).len(), 1);
        d.insert(
            b"DecodeParms".to_vec(),
            Object::Array(vec![
                Object::Dictionary(sub.clone()),
                Object::Integer(0), // becomes an empty dict
            ]),
        );
        assert_eq!(collect_parms(&d).len(), 2);
    }

    #[test]
    fn ascii_hex_filter_decodes() {
        // "Hi" — also exercises whitespace skipping and the early `>` exit.
        let out = decode_ascii_hex(b"48 6 9> trailing junk");
        assert_eq!(out, b"Hi");
        // Trailing single nibble pads with zero.
        let out = decode_ascii_hex(b"4");
        assert_eq!(out, vec![0x40]);
        // Mixed-case A-F exercises both alphabetic arms of the match.
        assert_eq!(decode_ascii_hex(b"deADBeEf"), vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn ascii85_filter_decodes() {
        // "Hello, world!" encoded with stock Ascii85, plus a trailing `~`
        // sentinel that the decoder should treat as end-of-data.
        let encoded = b"87cURD_*#TDfTZ)+T~>";
        let out = decode_ascii85(encoded);
        assert_eq!(out, b"Hello, world!");
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
    fn apply_filter_dispatch_covers_each_filter() {
        let empty = Dictionary::new();
        // FlateDecode (zlib of "hi")
        let zlib = [0x78, 0x9C, 0xCB, 0xC8, 0x04, 0x00, 0x01, 0x3D, 0x00, 0xD2];
        assert_eq!(apply_filter(b"FlateDecode", &zlib, &empty).unwrap(), b"hi");
        // Short alias `Fl`.
        assert_eq!(apply_filter(b"Fl", &zlib, &empty).unwrap(), b"hi");
        // ASCIIHex + short alias.
        assert_eq!(
            apply_filter(b"ASCIIHexDecode", b"4869>", &empty).unwrap(),
            b"Hi"
        );
        assert_eq!(apply_filter(b"AHx", b"4869>", &empty).unwrap(), b"Hi");
        // ASCII85 + short alias.
        let a85 = b"87cURD_*#TDfTZ)+T~>";
        assert_eq!(
            apply_filter(b"ASCII85Decode", a85, &empty).unwrap(),
            b"Hello, world!"
        );
        assert_eq!(apply_filter(b"A85", a85, &empty).unwrap(), b"Hello, world!");
        // Pass-through filters return data unchanged.
        for name in [
            b"DCTDecode".as_slice(),
            b"DCT",
            b"JPXDecode",
            b"CCITTFaxDecode",
            b"CCF",
        ] {
            assert_eq!(apply_filter(name, b"abc", &empty).unwrap(), b"abc");
        }
        // Unsupported filter errors.
        assert!(apply_filter(b"LZWDecode", b"abc", &empty).is_err());
    }

    // ---- PNG predictor --------------------------------------------------

    #[test]
    fn predictor_passes_through_when_disabled() {
        let mut p = Dictionary::new();
        p.insert(b"Predictor".to_vec(), Object::Integer(1));
        let raw = b"hello";
        assert_eq!(apply_predictor(raw, &p).unwrap(), raw);
    }

    #[test]
    fn predictor_decodes_each_png_filter() {
        // 3 columns, 1 colour, 8 bpc → row length 3 → stride 4.
        // Two rows, each with a different filter tag, decode back to the
        // same data we'd have if no predictor were in use.
        let mut params = Dictionary::new();
        params.insert(b"Predictor".to_vec(), Object::Integer(12));
        params.insert(b"Columns".to_vec(), Object::Integer(3));
        params.insert(b"Colors".to_vec(), Object::Integer(1));
        params.insert(b"BitsPerComponent".to_vec(), Object::Integer(8));

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
        let input: Vec<u8> = [f0.clone(), f1.clone()].concat();
        let decoded = apply_predictor(&input, &params).unwrap();
        assert_eq!(&decoded[..3], &r0);
        assert_eq!(&decoded[3..6], &r1);

        // tag 2 (Up): up reference is row above.
        let f0v: Vec<u8> = std::iter::once(2).chain(r0.iter().copied()).collect();
        let f1v: Vec<u8> = std::iter::once(2)
            .chain(r1.iter().zip(r0.iter()).map(|(a, b)| a.wrapping_sub(*b)))
            .collect();
        let input = [f0v, f1v].concat();
        let decoded = apply_predictor(&input, &params).unwrap();
        assert_eq!(&decoded[3..6], &r1);

        // tag 3 (Average) and tag 4 (Paeth) — round-trip a single zero row
        // with a known previous row so the helpers run end-to-end.
        let prev = [5u8, 10, 15];
        let next = [7u8, 12, 22];
        let f3: Vec<u8> = std::iter::once(3)
            .chain([
                next[0].wrapping_sub((prev[0] as u16 / 2) as u8),
                next[1].wrapping_sub(((next[0] as u16 + prev[1] as u16) / 2) as u8),
                next[2].wrapping_sub(((next[1] as u16 + prev[2] as u16) / 2) as u8),
            ])
            .collect();
        let prev_row: Vec<u8> = std::iter::once(0).chain(prev.iter().copied()).collect();
        let decoded = apply_predictor(&[prev_row, f3].concat(), &params).unwrap();
        assert_eq!(&decoded[3..6], &next);

        let f4: Vec<u8> = std::iter::once(4)
            .chain([
                next[0].wrapping_sub(paeth(0, prev[0], 0u8)),
                next[1].wrapping_sub(paeth(next[0], prev[1], prev[0])),
                next[2].wrapping_sub(paeth(next[1], prev[2], prev[1])),
            ])
            .collect();
        let prev_row: Vec<u8> = std::iter::once(0).chain(prev.iter().copied()).collect();
        let decoded = apply_predictor(&[prev_row, f4].concat(), &params).unwrap();
        assert_eq!(&decoded[3..6], &next);
    }

    #[test]
    fn predictor_unknown_tag_passes_raw_bytes_through() {
        let mut params = Dictionary::new();
        params.insert(b"Predictor".to_vec(), Object::Integer(12));
        params.insert(b"Columns".to_vec(), Object::Integer(2));
        // tag 99 hits the fallback arm.
        let input = [99u8, 1, 2];
        let decoded = apply_predictor(&input, &params).unwrap();
        assert_eq!(decoded, vec![1, 2]);
    }

    #[test]
    fn predictor_zero_row_returns_empty() {
        let mut params = Dictionary::new();
        params.insert(b"Predictor".to_vec(), Object::Integer(12));
        params.insert(b"Columns".to_vec(), Object::Integer(0));
        assert!(apply_predictor(&[1, 2, 3], &params).unwrap().is_empty());
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

    // ---- Helpers --------------------------------------------------------

    #[test]
    fn be_uint_collapses_byte_run() {
        assert_eq!(be_uint(&[0x12, 0x34, 0x56]), 0x123456);
        assert_eq!(be_uint(&[]), 0);
    }

    #[test]
    fn read_uint_handles_leading_whitespace_and_errors() {
        let bytes = b"  \t  42 next";
        let mut pos = 0;
        assert_eq!(read_uint(bytes, &mut pos).unwrap(), 42);
        // No digits: error.
        let bytes = b"   ";
        let mut pos = 0;
        assert!(read_uint(bytes, &mut pos).is_err());
    }

    #[test]
    fn skip_eol_handles_each_terminator() {
        // CRLF
        let bytes = b"  \r\n!";
        let mut pos = 0;
        skip_eol(bytes, &mut pos);
        assert_eq!(pos, 4);
        // bare CR
        let bytes = b"\r!";
        let mut pos = 0;
        skip_eol(bytes, &mut pos);
        assert_eq!(pos, 1);
        // bare LF
        let bytes = b"\n!";
        let mut pos = 0;
        skip_eol(bytes, &mut pos);
        assert_eq!(pos, 1);
        // No EOL — pos unchanged after leading-ws skip.
        let bytes = b"  !";
        let mut pos = 0;
        skip_eol(bytes, &mut pos);
        assert_eq!(pos, 2);
    }

    #[test]
    fn skip_inline_consumes_tabs_and_spaces_only() {
        let bytes = b" \t \n";
        let mut pos = 0;
        skip_inline(bytes, &mut pos);
        assert_eq!(pos, 3); // stops at the newline
    }

    #[test]
    fn deref_resolves_chains_and_handles_dead_refs() {
        // Build a tiny doc by hand: id 1 points at id 2 which is an int.
        let mut objs = HashMap::new();
        objs.insert(ObjectId(1, 0), Object::Reference(ObjectId(2, 0)));
        objs.insert(ObjectId(2, 0), Object::Integer(7));
        let doc = Document {
            objects: objs,
            pages: vec![],
        };
        let obj = doc.get_object(ObjectId(1, 0)).unwrap();
        assert_eq!(doc.deref(obj).as_integer(), Some(7));

        // Dangling reference: deref returns the unresolved reference itself.
        let dangling = Object::Reference(ObjectId(999, 0));
        assert!(doc.deref(&dangling).as_reference().is_some());
    }

    #[test]
    fn page_content_supports_array_of_streams() {
        // Build a doc where /Contents is an array of two stream refs.
        let mut objs = HashMap::new();
        let stream1 = Object::Stream(Stream {
            dict: Dictionary::new(),
            content: b"first".to_vec(),
        });
        let stream2 = Object::Stream(Stream {
            dict: Dictionary::new(),
            content: b"second".to_vec(),
        });
        objs.insert(ObjectId(10, 0), stream1);
        objs.insert(ObjectId(11, 0), stream2);
        let mut page = Dictionary::new();
        page.insert(
            b"Contents".to_vec(),
            Object::Array(vec![
                Object::Reference(ObjectId(10, 0)),
                Object::Reference(ObjectId(11, 0)),
            ]),
        );
        objs.insert(ObjectId(20, 0), Object::Dictionary(page));
        let doc = Document {
            objects: objs,
            pages: vec![ObjectId(20, 0)],
        };
        let content = doc.get_page_content(ObjectId(20, 0)).unwrap();
        // Two stream bodies joined by a newline (since neither ends in
        // whitespace).
        assert_eq!(content, b"first\nsecond");
    }

    #[test]
    fn get_page_content_returns_none_for_unknown_page_id() {
        let doc = Document {
            objects: HashMap::new(),
            pages: vec![],
        };
        assert!(doc.get_page_content(ObjectId(99, 0)).is_none());
    }

    #[test]
    fn page_content_array_skips_streams_that_fail_to_decode() {
        // Two streams in /Contents: the first has a corrupt FlateDecode
        // body (decode_filters returns Err), the second is fine. Only the
        // valid one shows up in the joined output.
        let mut objs = HashMap::new();
        let mut bad_dict = Dictionary::new();
        bad_dict.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
        objs.insert(
            ObjectId(10, 0),
            Object::Stream(Stream {
                dict: bad_dict,
                content: b"NOT-VALID-ZLIB".to_vec(),
            }),
        );
        objs.insert(
            ObjectId(11, 0),
            Object::Stream(Stream {
                dict: Dictionary::new(),
                content: b"GOOD".to_vec(),
            }),
        );
        let mut page = Dictionary::new();
        page.insert(
            b"Contents".to_vec(),
            Object::Array(vec![
                Object::Reference(ObjectId(10, 0)),
                Object::Reference(ObjectId(11, 0)),
            ]),
        );
        objs.insert(ObjectId(20, 0), Object::Dictionary(page));
        let doc = Document {
            objects: objs,
            pages: vec![ObjectId(20, 0)],
        };
        let content = doc.get_page_content(ObjectId(20, 0)).unwrap();
        assert_eq!(content, b"GOOD");
    }

    #[test]
    fn classic_xref_with_non_integer_count_errors() {
        // The xref subsection header reads "first count" — replace count
        // with a non-digit so read_uint errors.
        let body = b"%PDF-1.4\n";
        let mut out = body.to_vec();
        let xref_offset = out.len();
        let mut xref = String::from("xref\n0 BAD\n");
        xref.push_str(&format!(
            "trailer <</Size 0/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n"
        ));
        out.extend_from_slice(xref.as_bytes());
        assert!(Document::load(&out).is_err());
    }

    #[test]
    fn classic_xref_skips_already_known_entries_on_prev_chain() {
        // Two xref sections (an "old" one referenced via /Prev and the
        // current one). They both list obj 1; the most recent xref wins
        // and the older entry is skipped via `continue`.
        let mut body = String::from("%PDF-1.4\n");
        let off1_a = body.len();
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        let off2 = body.len();
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        let off3 = body.len();
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        // Older xref (just obj 1 at off1_a, plus free entry).
        let prev_xref_offset = body.len();
        body.push_str("xref\n0 2\n0000000000 65535 f \n");
        body.push_str(&format!("{off1_a:010} 00000 n \n"));
        body.push_str("trailer <</Size 2/Root 1 0 R>>\nstartxref\n");
        body.push_str(&format!("{prev_xref_offset}\n%%EOF\n"));
        // Newer copy of obj 1 (functionally identical) plus a new xref.
        let off1_b = body.len();
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        let xref_offset = body.len();
        body.push_str("xref\n0 4\n0000000000 65535 f \n");
        body.push_str(&format!("{off1_b:010} 00000 n \n"));
        body.push_str(&format!("{off2:010} 00000 n \n"));
        body.push_str(&format!("{off3:010} 00000 n \n"));
        body.push_str(&format!(
            "trailer <</Size 4/Root 1 0 R/Prev {prev_xref_offset}>>\nstartxref\n{xref_offset}\n%%EOF\n"
        ));
        let doc = Document::load(body.as_bytes()).expect("load");
        assert_eq!(doc.pages().len(), 1);
    }

    #[test]
    fn classic_xref_with_malformed_trailer_object_errors() {
        // The `trailer` keyword is present but is followed by an
        // unparseable byte stream (no `<<...>>` dict).
        let body = b"%PDF-1.4\n";
        let mut out = body.to_vec();
        let xref_offset = out.len();
        out.extend_from_slice(
            b"xref\n0 1\n0000000000 65535 f \ntrailer @@@\nstartxref\n9\n%%EOF\n",
        );
        let _ = xref_offset;
        assert!(Document::load(&out).is_err());
    }

    #[test]
    fn classic_xref_with_non_integer_first_errors() {
        // The very first integer in the subsection header is non-numeric,
        // so the `first` read_uint call errors before we ever reach count.
        let body = b"%PDF-1.4\n";
        let mut out = body.to_vec();
        let xref_offset = out.len();
        let mut xref = String::from("xref\nBAD 1\n");
        xref.push_str(&format!(
            "trailer <</Size 0/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n"
        ));
        out.extend_from_slice(xref.as_bytes());
        assert!(Document::load(&out).is_err());
    }

    #[test]
    fn document_load_skips_uncompressed_entries_that_fail_to_parse() {
        // The catalog/pages/page chain is valid, but the xref also lists
        // an obj 4 at a bogus byte offset where no `4 0 obj` header
        // actually starts. parse_at returns None and the entry is
        // silently dropped.
        let body = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
";
        let mut out = body.to_vec();
        let xref_offset = out.len();
        let offsets: Vec<usize> = (1..=3)
            .map(|n| {
                let needle = format!("{n} 0 obj");
                find_subslice(&out, needle.as_bytes()).unwrap()
            })
            .collect();
        let mut xref = String::from("xref\n0 5\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        // Entry 4: offset 5 — points into the middle of `%PDF-1.4` where
        // there is no indirect-object header. parse_at returns None.
        xref.push_str("0000000005 00000 n \n");
        xref.push_str(&format!(
            "trailer <</Size 5/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n"
        ));
        out.extend_from_slice(xref.as_bytes());
        let doc = Document::load(&out).expect("load");
        // The non-existent obj 4 was dropped, but the catalog/pages/page
        // chain is intact.
        assert_eq!(doc.pages().len(), 1);
        assert!(doc.get_object(ObjectId(4, 0)).is_none());
    }

    #[test]
    fn document_load_propagates_objstm_decode_error() {
        // Build a PDF whose object stream has /Filter /FlateDecode but a
        // garbage body. decode_filters errors → propagates through
        // Document::load via the `?` on line 101.
        let mut body = String::from("%PDF-1.5\n");
        let off1 = body.len();
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        let off2 = body.len();
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        let off3 = body.len();
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        let off4 = body.len();
        body.push_str("4 0 obj <</Type/ObjStm/N 1/First 4/Filter/FlateDecode/Length 4>>\nstream\nJUNK\nendstream endobj\n");
        let xref_offset = body.len();
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        for &off in &[off1, off2, off3, off4] {
            payload.push(1);
            payload.extend_from_slice(&(off as u16).to_be_bytes());
            payload.push(0);
        }
        // entry 5: compressed in objstm 4 (which will fail to decode).
        payload.push(2);
        payload.extend_from_slice(&4u16.to_be_bytes());
        payload.push(0);
        // entry 6: xref stream itself.
        payload.push(1);
        payload.extend_from_slice(&(xref_offset as u16).to_be_bytes());
        payload.push(0);
        body.push_str(&format!(
            "6 0 obj <</Type/XRef/Size 7/Root 1 0 R/W [1 2 1]/Length {}>>\nstream\n",
            payload.len()
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        assert!(Document::load(&bytes).is_err());
    }

    #[test]
    fn document_load_falls_back_to_index_based_objstm_lookup() {
        // Some producers don't put the actual object id in the objstm
        // header. The xref references obj 1 as compressed[0] in objstm 4,
        // but the body's header claims it's obj 99. The find-by-number
        // misses, and the index-based fallback should still pick up the
        // payload at slot 0.
        let obj_body = "<</Type/Catalog/Pages 2 0 R>>";
        let mismatched_header = "99 0 ".to_string();
        let first = mismatched_header.len();
        let mut objstm_payload = mismatched_header.into_bytes();
        objstm_payload.extend_from_slice(obj_body.as_bytes());

        let mut body = String::from("%PDF-1.5\n");
        let off2 = body.len();
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        let off3 = body.len();
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        let off4 = body.len();
        body.push_str(&format!(
            "4 0 obj <</Type/ObjStm/N 1/First {first}/Length {}>>\nstream\n",
            objstm_payload.len()
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(&objstm_payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        let xref_offset = bytes.len();
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]); // entry 0 free
                                                  // entry 1: catalog — compressed in objstm 4 at index 0. The body
                                                  // header claims it's obj 99; the loader's index fallback rescues us.
        payload.push(2);
        payload.extend_from_slice(&4u16.to_be_bytes());
        payload.push(0);
        for &off in &[off2, off3, off4] {
            payload.push(1);
            payload.extend_from_slice(&(off as u16).to_be_bytes());
            payload.push(0);
        }
        // entry 5: xref stream itself.
        payload.push(1);
        payload.extend_from_slice(&(xref_offset as u16).to_be_bytes());
        payload.push(0);
        bytes.extend_from_slice(
            format!(
                "5 0 obj <</Type/XRef/Size 6/Root 1 0 R/W [1 2 1]/Length {}>>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        let doc = Document::load(&bytes).expect("load");
        assert_eq!(doc.pages().len(), 1);
    }

    #[test]
    fn document_load_propagates_objstm_header_error() {
        // Objstm with /N and /First set but the body's header has a
        // non-numeric obj id → parse_object_stream returns Err and the
        // outer `?` propagates it.
        let mut body = String::from("%PDF-1.5\n");
        let off1 = body.len();
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        let off2 = body.len();
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        let off3 = body.len();
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        let objstm_body = b"BAD 0 (oops)";
        let off4 = body.len();
        body.push_str(&format!(
            "4 0 obj <</Type/ObjStm/N 1/First 6/Length {}>>\nstream\n",
            objstm_body.len()
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(objstm_body);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        let xref_offset = bytes.len();
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        for &off in &[off1, off2, off3, off4] {
            payload.push(1);
            payload.extend_from_slice(&(off as u16).to_be_bytes());
            payload.push(0);
        }
        payload.push(2);
        payload.extend_from_slice(&4u16.to_be_bytes());
        payload.push(0);
        payload.push(1);
        payload.extend_from_slice(&(xref_offset as u16).to_be_bytes());
        payload.push(0);
        bytes.extend_from_slice(
            format!(
                "6 0 obj <</Type/XRef/Size 7/Root 1 0 R/W [1 2 1]/Length {}>>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        assert!(Document::load(&bytes).is_err());
    }

    #[test]
    fn parse_object_stream_errors_when_header_has_non_digit() {
        let mut dict = Dictionary::new();
        dict.insert(b"N".to_vec(), Object::Integer(1));
        dict.insert(b"First".to_vec(), Object::Integer(5));
        // Body header "ABC 0 " — read_uint can't parse the first token.
        let body = b"ABC 0 (hi)";
        assert!(parse_object_stream(&dict, body).is_err());
    }

    #[test]
    fn document_load_reuses_objstm_cache_across_compressed_entries() {
        // Two compressed entries pointing into the same objstm. The cache
        // lookup hits on the second entry (cache Some(v) branch).
        let obj1 = "<</Type/Catalog/Pages 2 0 R>>";
        let obj2 = "<</Type/Pages/Kids[3 0 R]/Count 1>>";
        let obj3 = "<</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>>";
        let header = format!(
            "1 0 2 {o2} 3 {o3} ",
            o2 = obj1.len(),
            o3 = obj1.len() + obj2.len()
        );
        let first = header.len();
        let mut objstm_payload = header.into_bytes();
        objstm_payload.extend_from_slice(obj1.as_bytes());
        objstm_payload.extend_from_slice(obj2.as_bytes());
        objstm_payload.extend_from_slice(obj3.as_bytes());

        let mut body = String::from("%PDF-1.5\n");
        let off4 = body.len();
        body.push_str(&format!(
            "4 0 obj <</Type/ObjStm/N 3/First {first}/Length {}>>\nstream\n",
            objstm_payload.len()
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(&objstm_payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        let xref_offset = bytes.len();
        // Three compressed entries (1, 2, 3) all pointing at objstm 4.
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]); // entry 0 free
        for index in 0u8..3 {
            payload.push(2);
            payload.extend_from_slice(&4u16.to_be_bytes());
            payload.push(index);
        }
        payload.push(1);
        payload.extend_from_slice(&(off4 as u16).to_be_bytes());
        payload.push(0);
        bytes.extend_from_slice(
            format!(
                "5 0 obj <</Type/XRef/Size 5/Root 1 0 R/W [1 2 1]/Length {}>>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        let doc = Document::load(&bytes).expect("load");
        assert_eq!(doc.pages().len(), 1);
    }

    #[test]
    fn page_content_returns_none_when_contents_is_missing_or_unsupported() {
        let mut objs = HashMap::new();
        let page = Dictionary::new();
        objs.insert(ObjectId(1, 0), Object::Dictionary(page));
        let doc = Document {
            objects: objs.clone(),
            pages: vec![],
        };
        assert!(doc.get_page_content(ObjectId(1, 0)).is_none());

        let mut page = Dictionary::new();
        // /Contents → integer is unsupported and yields None.
        page.insert(b"Contents".to_vec(), Object::Integer(42));
        objs.insert(ObjectId(1, 0), Object::Dictionary(page));
        let doc = Document {
            objects: objs,
            pages: vec![],
        };
        assert!(doc.get_page_content(ObjectId(1, 0)).is_none());
    }

    // ---- Object streams -------------------------------------------------

    #[test]
    fn parse_object_stream_returns_each_entry() {
        // Two objects: "(hi) endobj"-style payloads.
        let mut dict = Dictionary::new();
        dict.insert(b"N".to_vec(), Object::Integer(2));
        dict.insert(b"First".to_vec(), Object::Integer(10));
        // Header: "10 0 11 4" — obj #10 at offset 0, obj #11 at offset 4.
        // Body: "(hi)" then "(by)".
        let body = b"10 0 11 4 (hi)(by)";
        let entries = parse_object_stream(&dict, body).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, 10);
        assert_eq!(entries[0].1, b"(hi)");
        assert_eq!(entries[1].0, 11);
        assert_eq!(entries[1].1, b"(by)");
    }

    #[test]
    fn parse_object_stream_errors_without_required_keys() {
        let dict = Dictionary::new();
        assert!(parse_object_stream(&dict, b"").is_err());
        let mut dict = Dictionary::new();
        dict.insert(b"N".to_vec(), Object::Integer(0));
        assert!(parse_object_stream(&dict, b"").is_err());
    }

    #[test]
    fn parse_object_stream_rejects_negative_or_huge_n() {
        // A negative /N cast through `as usize` is ~1.8×10^19 and would
        // abort the allocator on Vec::with_capacity below.
        let mut dict = Dictionary::new();
        dict.insert(b"N".to_vec(), Object::Integer(-1));
        dict.insert(b"First".to_vec(), Object::Integer(0));
        let err = parse_object_stream(&dict, b"").unwrap_err();
        assert!(err.to_string().contains("/N out of range"));

        // /N larger than the decoded payload can possibly hold.
        let mut dict = Dictionary::new();
        dict.insert(b"N".to_vec(), Object::Integer(i64::MAX));
        dict.insert(b"First".to_vec(), Object::Integer(0));
        let err = parse_object_stream(&dict, b"short").unwrap_err();
        assert!(err.to_string().contains("/N out of range"));
    }

    #[test]
    fn parse_object_stream_rejects_negative_or_huge_first() {
        let mut dict = Dictionary::new();
        dict.insert(b"N".to_vec(), Object::Integer(0));
        dict.insert(b"First".to_vec(), Object::Integer(-1));
        let err = parse_object_stream(&dict, b"").unwrap_err();
        assert!(err.to_string().contains("/First out of range"));

        let mut dict = Dictionary::new();
        dict.insert(b"N".to_vec(), Object::Integer(0));
        dict.insert(b"First".to_vec(), Object::Integer(i64::MAX));
        let err = parse_object_stream(&dict, b"short").unwrap_err();
        assert!(err.to_string().contains("/First out of range"));
    }

    // ---- Xref streams ---------------------------------------------------

    /// Compose a 1-object PDF body whose only indirect object is an xref
    /// stream with the entries described by `entries`. The body has no
    /// `/Filter`, so the encoded rows are written raw and live inside the
    /// stream payload.
    fn xref_stream_pdf(entries: &[(u8, u64, u32)], extra_dict_entries: &str) -> Vec<u8> {
        let mut payload: Vec<u8> = Vec::new();
        for (kind, f1, f2) in entries {
            payload.push(*kind);
            payload.extend_from_slice(&(*f1 as u16).to_be_bytes());
            payload.push(*f2 as u8);
        }
        let dict = format!(
            "<</Type/XRef/Size {}/W [1 2 1]/Length {}{}>>",
            entries.len(),
            payload.len(),
            extra_dict_entries,
        );
        let body = format!("1 0 obj {dict}\nstream\n");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(body.as_bytes());
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes
    }

    #[test]
    fn read_xref_stream_recognises_every_entry_kind() {
        let entries = &[
            (0u8, 0u64, 0u32),   // free
            (1u8, 100u64, 0u32), // uncompressed at offset 100
            (2u8, 99u64, 3u32),  // compressed: lives in objstm 99, idx 3
            (5u8, 0u64, 0u32),   // unknown kind — silently skipped
        ];
        let bytes = xref_stream_pdf(entries, "");
        let mut out = BTreeMap::new();
        let dict = read_xref_stream(&bytes, 0, &mut out).unwrap();
        assert!(dict.get(b"Type").is_some());
        // Debug-format comparisons sidestep the dead arms a `matches!`
        // expansion would introduce.
        assert_eq!(format!("{:?}", out.get(&ObjectId(0, 0)).unwrap()), "Free");
        assert_eq!(
            format!("{:?}", out.get(&ObjectId(1, 0)).unwrap()),
            "Uncompressed { offset: 100 }",
        );
        assert_eq!(
            format!("{:?}", out.get(&ObjectId(2, 0)).unwrap()),
            "Compressed { stream_obj: 99, index: 3 }",
        );
        assert!(!out.contains_key(&ObjectId(3, 0)));
    }

    #[test]
    fn read_xref_stream_honours_index_chunks() {
        // Two-entry stream describing IDs starting at 10.
        let entries = &[(1u8, 10u64, 0u32), (1u8, 20u64, 0u32)];
        let bytes = xref_stream_pdf(entries, "/Index [10 2]");
        let mut out = BTreeMap::new();
        read_xref_stream(&bytes, 0, &mut out).unwrap();
        assert!(out.contains_key(&ObjectId(10, 0)));
        assert!(out.contains_key(&ObjectId(11, 0)));
    }

    #[test]
    fn read_xref_stream_existing_entry_wins() {
        let entries = &[(1u8, 200u64, 0u32)];
        let bytes = xref_stream_pdf(entries, "");
        let mut out = BTreeMap::new();
        out.insert(ObjectId(0, 0), XrefEntry::Uncompressed { offset: 999 });
        read_xref_stream(&bytes, 0, &mut out).unwrap();
        // Pre-existing entry isn't overwritten.
        assert_eq!(
            format!("{:?}", out.get(&ObjectId(0, 0)).unwrap()),
            "Uncompressed { offset: 999 }",
        );
    }

    #[test]
    fn read_xref_stream_errors_on_bad_w_length() {
        let body = b"1 0 obj <</Type/XRef/Size 1/W [1 2]/Length 0>>\nstream\n\nendstream endobj\n";
        let mut out = BTreeMap::new();
        assert!(read_xref_stream(body, 0, &mut out).is_err());
    }

    #[test]
    fn read_xref_stream_errors_on_zero_row_width() {
        let body =
            b"1 0 obj <</Type/XRef/Size 1/W [0 0 0]/Length 0>>\nstream\n\nendstream endobj\n";
        let mut out = BTreeMap::new();
        assert!(read_xref_stream(body, 0, &mut out).is_err());
    }

    #[test]
    fn read_xref_stream_errors_when_size_missing() {
        let body = b"1 0 obj <</Type/XRef/W [1 2 1]/Length 4>>\nstream\n\x01\x00\x10\x00\nendstream endobj\n";
        let mut out = BTreeMap::new();
        assert!(read_xref_stream(body, 0, &mut out).is_err());
    }

    #[test]
    fn read_xref_stream_errors_on_truncated_payload() {
        // /W = 1+2+1 = 4, /Size = 2 → expects 8 bytes. We give 4.
        let body = b"1 0 obj <</Type/XRef/Size 2/W [1 2 1]/Length 4>>\nstream\n\x01\x00\x10\x00\nendstream endobj\n";
        let mut out = BTreeMap::new();
        assert!(read_xref_stream(body, 0, &mut out).is_err());
    }

    #[test]
    fn read_xref_stream_errors_when_w_missing() {
        let body = b"1 0 obj <</Type/XRef/Size 1/Length 0>>\nstream\n\nendstream endobj\n";
        let mut out = BTreeMap::new();
        assert!(read_xref_stream(body, 0, &mut out).is_err());
    }

    #[test]
    fn read_xref_stream_errors_when_object_is_not_a_stream() {
        let body = b"1 0 obj 42 endobj\n";
        let mut out = BTreeMap::new();
        assert!(read_xref_stream(body, 0, &mut out).is_err());
    }

    // ---- End-to-end variants --------------------------------------------

    #[test]
    fn loads_pdf_with_xref_stream_root() {
        // Build a minimal PDF whose startxref points at an xref stream.
        // Layout:
        //   obj 1: catalog
        //   obj 2: pages
        //   obj 3: page
        //   obj 4: xref stream describing obj 0..4
        //   startxref → offset of obj 4
        let mut body = String::from("%PDF-1.5\n");
        let off1 = body.len();
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        let off2 = body.len();
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        let off3 = body.len();
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        let xref_offset = body.len();
        // Build the 5-entry payload (free + obj 1..=4). Field widths 1/2/1.
        let mut payload: Vec<u8> = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]); // entry 0: free
        for &off in &[off1, off2, off3, xref_offset] {
            payload.push(1);
            payload.extend_from_slice(&(off as u16).to_be_bytes());
            payload.push(0);
        }
        body.push_str(&format!(
            "4 0 obj <</Type/XRef/Size 5/Root 1 0 R/W [1 2 1]/Length {}>>\nstream\n",
            payload.len()
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        let doc = Document::load(&bytes).expect("load");
        assert_eq!(doc.pages().len(), 1);
    }

    #[test]
    fn page_tree_walk_rejects_excessive_depth() {
        // Construct objects forming a self-referential /Pages cycle that
        // the recursion guard should bail out of.
        let mut objects = HashMap::new();
        let mut pages = Dictionary::new();
        pages.insert(
            b"Kids".to_vec(),
            Object::Array(vec![Object::Reference(ObjectId(1, 0))]),
        );
        objects.insert(ObjectId(1, 0), Object::Dictionary(pages));
        let mut out = Vec::new();
        let err = walk_pages(&objects, ObjectId(1, 0), &mut out, 65);
        assert!(err.is_err());
    }

    #[test]
    fn collect_pages_errors_when_trailer_root_missing() {
        let objects = HashMap::new();
        let trailer = Dictionary::new();
        assert!(collect_pages(&objects, &trailer).is_err());
    }

    #[test]
    fn collect_pages_errors_when_catalog_missing() {
        let mut trailer = Dictionary::new();
        trailer.insert(b"Root".to_vec(), Object::Reference(ObjectId(1, 0)));
        let objects = HashMap::new();
        assert!(collect_pages(&objects, &trailer).is_err());
    }

    #[test]
    fn collect_pages_errors_when_pages_missing_in_catalog() {
        let mut catalog = Dictionary::new();
        catalog.insert(b"Type".to_vec(), Object::Name(b"Catalog".to_vec()));
        let mut objects = HashMap::new();
        objects.insert(ObjectId(1, 0), Object::Dictionary(catalog));
        let mut trailer = Dictionary::new();
        trailer.insert(b"Root".to_vec(), Object::Reference(ObjectId(1, 0)));
        assert!(collect_pages(&objects, &trailer).is_err());
    }

    #[test]
    fn deref_terminates_on_cycle() {
        // A cyclic reference (1→2→1) should bottom out rather than loop.
        let mut objs = HashMap::new();
        objs.insert(ObjectId(1, 0), Object::Reference(ObjectId(2, 0)));
        objs.insert(ObjectId(2, 0), Object::Reference(ObjectId(1, 0)));
        let doc = Document {
            objects: objs,
            pages: vec![],
        };
        let start = Object::Reference(ObjectId(1, 0));
        // We don't care which id we land on — just that we terminate at a
        // Reference (the cycle never reaches a concrete value).
        assert!(doc.deref(&start).as_reference().is_some());
    }

    #[test]
    fn load_errors_on_pdf_with_no_root() {
        // Header is valid, xref is valid, trailer has no /Root.
        let body = b"\
%PDF-1.4
1 0 obj <</Type/Pages/Kids[2 0 R]/Count 1>> endobj
2 0 obj <</Type/Page/Parent 1 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
";
        let mut out = body.to_vec();
        let xref_offset = out.len();
        let offsets: Vec<usize> = (1..=2)
            .map(|n| {
                let needle = format!("{n} 0 obj");
                find_subslice(&out, needle.as_bytes()).unwrap()
            })
            .collect();
        let mut xref = String::from("xref\n0 3\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str("trailer <</Size 3>>\nstartxref\n");
        xref.push_str(&format!("{xref_offset}\n%%EOF\n"));
        out.extend_from_slice(xref.as_bytes());
        assert!(Document::load(&out).is_err());
    }

    #[test]
    fn loads_pdf_with_object_stream() {
        // PDF 1.5+ layout: catalog/pages/page packed into an object stream,
        // referenced from an xref stream with type-2 entries.
        // We hand-build all of it so we can exercise the cache-miss path
        // in Document::load that decodes the object stream and pulls
        // individual objects out of it.
        let mut body = String::from("%PDF-1.5\n");
        // Object stream payload — three indirect objects.
        let obj1 = "<</Type/Catalog/Pages 2 0 R>>";
        let obj2 = "<</Type/Pages/Kids[3 0 R]/Count 1>>";
        let obj3 = "<</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>>";
        // Header: "obj_num offset" pairs, then objects concatenated.
        let header = format!(
            "1 0 2 {o2} 3 {o3} ",
            o2 = obj1.len(),
            o3 = obj1.len() + obj2.len()
        );
        let first = header.len();
        let mut objstm_payload = header.into_bytes();
        objstm_payload.extend_from_slice(obj1.as_bytes());
        objstm_payload.extend_from_slice(obj2.as_bytes());
        objstm_payload.extend_from_slice(obj3.as_bytes());

        let off4 = body.len();
        body.push_str(&format!(
            "4 0 obj <</Type/ObjStm/N 3/First {first}/Length {}>>\nstream\n",
            objstm_payload.len(),
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(&objstm_payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");

        // Xref stream covers ids 0..=4. Entries:
        //   0: free
        //   1,2,3: compressed in object 4 at indices 0,1,2
        //   4: uncompressed at off4
        let xref_offset = bytes.len();
        let mut payload: Vec<u8> = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]); // entry 0: free
        for index in 0u8..3 {
            payload.push(2);
            payload.extend_from_slice(&4u16.to_be_bytes()); // stream_obj = 4
            payload.push(index);
        }
        payload.push(1);
        payload.extend_from_slice(&(off4 as u16).to_be_bytes());
        payload.push(0);

        bytes.extend_from_slice(
            format!(
                "5 0 obj <</Type/XRef/Size 5/Root 1 0 R/W [1 2 1]/Length {}>>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        let doc = Document::load(&bytes).expect("load");
        assert_eq!(doc.pages().len(), 1);
    }

    // ---- Failure paths through Document::load --------------------------

    #[test]
    fn document_load_propagates_startxref_failure() {
        // Header is valid but the file is missing the `startxref` marker.
        let bytes = b"%PDF-1.4\nnot a real pdf";
        assert!(Document::load(bytes).is_err());
    }

    #[test]
    fn document_load_propagates_xref_chain_failure() {
        // startxref points past the end of the file → read_xref_chain errors.
        let bytes = b"%PDF-1.4\nstartxref\n9999\n%%EOF";
        assert!(Document::load(bytes).is_err());
    }

    #[test]
    fn document_load_skips_compressed_objects_with_missing_objstm() {
        // Build a classic-xref PDF that references obj 5 as if it lived in
        // obj 99's object stream, but obj 99 doesn't exist. The loader
        // should silently skip the missing-objstm entries and still
        // succeed for the remaining objects.
        let mut body = String::from("%PDF-1.5\n");
        let off1 = body.len();
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        let off2 = body.len();
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        let off3 = body.len();
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        let xref_offset = body.len();
        // Build an xref stream that has a compressed entry pointing at a
        // non-existent objstm (id 99). The loader should `continue` rather
        // than crash.
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]); // entry 0: free
        for &off in &[off1, off2, off3] {
            payload.push(1);
            payload.extend_from_slice(&(off as u16).to_be_bytes());
            payload.push(0);
        }
        // entry 4 is a compressed reference to a missing objstm 99.
        payload.push(2);
        payload.extend_from_slice(&99u16.to_be_bytes());
        payload.push(0);
        // entry 5 is the xref stream itself, lives at xref_offset.
        payload.push(1);
        payload.extend_from_slice(&(xref_offset as u16).to_be_bytes());
        payload.push(0);
        body.push_str(&format!(
            "5 0 obj <</Type/XRef/Size 6/Root 1 0 R/W [1 2 1]/Length {}>>\nstream\n",
            payload.len()
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        let doc = Document::load(&bytes).expect("load");
        // The good page survives even though obj 4 was unreachable.
        assert_eq!(doc.pages().len(), 1);
    }

    #[test]
    fn classic_xref_with_unknown_entry_kind_is_skipped() {
        // Build a classic xref whose third entry uses kind 'x' (not n/f).
        // The loader should silently skip it without erroring.
        let body = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
";
        let mut out = body.to_vec();
        let xref_offset = out.len();
        let offsets: Vec<usize> = (1..=3)
            .map(|n| {
                let needle = format!("{n} 0 obj");
                find_subslice(&out, needle.as_bytes()).unwrap()
            })
            .collect();
        // The fourth entry uses kind 'x' instead of 'n' or 'f'.
        let mut xref = String::from("xref\n0 5\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str("0000099999 00000 x \n");
        xref.push_str(&format!(
            "trailer <</Size 5/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n"
        ));
        out.extend_from_slice(xref.as_bytes());
        let doc = Document::load(&out).expect("load");
        assert_eq!(doc.pages().len(), 1);
    }

    #[test]
    fn classic_xref_with_truncated_entry_errors() {
        // Header is correct, but the xref entries are cut short.
        let bytes =
            b"%PDF-1.4\nxref\n0 5\n0000000000 65535 f \n0000000010 00000 n\nstartxref\n9\n%%EOF";
        assert!(Document::load(bytes).is_err());
    }

    #[test]
    fn classic_xref_with_non_dict_trailer_errors() {
        // Trailer keyword is followed by a number instead of a dict.
        let body = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
";
        let mut out = body.to_vec();
        let xref_offset = out.len();
        let off1 = find_subslice(&out, b"1 0 obj").unwrap();
        let mut xref = String::from("xref\n0 2\n0000000000 65535 f \n");
        xref.push_str(&format!("{off1:010} 00000 n \n"));
        xref.push_str(&format!("trailer 42\nstartxref\n{xref_offset}\n%%EOF\n"));
        out.extend_from_slice(xref.as_bytes());
        assert!(Document::load(&out).is_err());
    }

    #[test]
    fn xref_stream_with_zero_type_width_defaults_to_one() {
        // /W [0 2 1] omits the type field — spec says it defaults to 1
        // (uncompressed). Verify by building such a stream and checking the
        // entries that come out.
        let bytes = b"1 0 obj <</Type/XRef/Size 1/W [0 2 1]/Length 3>>\nstream\n\x00\x10\x00\nendstream endobj\n";
        let mut out = BTreeMap::new();
        read_xref_stream(bytes, 0, &mut out).unwrap();
        assert_eq!(
            format!("{:?}", out.get(&ObjectId(0, 0)).unwrap()),
            "Uncompressed { offset: 16 }",
        );
    }

    #[test]
    fn xref_stream_with_odd_index_chunk_breaks_loop() {
        // /Index has 3 entries (not a multiple of 2) — the trailing single
        // entry should break the chunk loop.
        let bytes = b"1 0 obj <</Type/XRef/Size 1/W [1 2 1]/Index [0 1 5]/Length 4>>\nstream\n\x01\x00\x10\x00\nendstream endobj\n";
        let mut out = BTreeMap::new();
        read_xref_stream(bytes, 0, &mut out).unwrap();
        // Only the first chunk was consumed.
        assert!(out.contains_key(&ObjectId(0, 0)));
        assert!(!out.contains_key(&ObjectId(5, 0)));
    }

    #[test]
    fn xref_stream_with_inflate_failure_errors() {
        // Filter is FlateDecode but the body isn't valid zlib.
        let bytes =
            b"1 0 obj <</Type/XRef/Size 1/W [1 2 1]/Filter/FlateDecode/Length 4>>\nstream\nJUNK\nendstream endobj\n";
        let mut out = BTreeMap::new();
        assert!(read_xref_stream(bytes, 0, &mut out).is_err());
    }

    #[test]
    fn page_content_returns_empty_for_array_with_no_streams() {
        // Page whose /Contents array points only at non-streams produces
        // an empty body rather than failing.
        let mut objs = HashMap::new();
        objs.insert(ObjectId(2, 0), Object::Integer(7));
        let mut page = Dictionary::new();
        page.insert(
            b"Contents".to_vec(),
            Object::Array(vec![Object::Reference(ObjectId(2, 0))]),
        );
        objs.insert(ObjectId(1, 0), Object::Dictionary(page));
        let doc = Document {
            objects: objs,
            pages: vec![ObjectId(1, 0)],
        };
        assert_eq!(doc.get_page_content(ObjectId(1, 0)), Some(Vec::new()));
    }

    #[test]
    fn walk_pages_returns_ok_for_missing_or_non_dict_node() {
        let objs = HashMap::new();
        let mut out = Vec::new();
        // Node id doesn't exist → early Ok return.
        walk_pages(&objs, ObjectId(99, 0), &mut out, 0).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn walk_pages_pushes_leaf_when_called_with_type_page_directly() {
        // /Pages reference in the catalog points at a /Type Page leaf —
        // unusual but valid. walk_pages should push it without recursing.
        let mut objs = HashMap::new();
        let mut page = Dictionary::new();
        page.insert(b"Type".to_vec(), Object::Name(b"Page".to_vec()));
        objs.insert(ObjectId(1, 0), Object::Dictionary(page));
        let mut out = Vec::new();
        walk_pages(&objs, ObjectId(1, 0), &mut out, 0).unwrap();
        assert_eq!(out, vec![ObjectId(1, 0)]);
    }

    #[test]
    fn walk_pages_returns_ok_when_pages_node_has_no_kids() {
        // A /Type Pages node without /Kids is silently treated as empty.
        let mut objs = HashMap::new();
        let mut pages = Dictionary::new();
        pages.insert(b"Type".to_vec(), Object::Name(b"Pages".to_vec()));
        objs.insert(ObjectId(1, 0), Object::Dictionary(pages));
        let mut out = Vec::new();
        walk_pages(&objs, ObjectId(1, 0), &mut out, 0).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn walk_pages_recurses_into_kids_pointing_at_missing_node() {
        // A /Kids reference to a non-existent id should propagate as an
        // Ok no-op rather than blowing up.
        let mut objs = HashMap::new();
        let mut pages = Dictionary::new();
        pages.insert(b"Type".to_vec(), Object::Name(b"Pages".to_vec()));
        pages.insert(
            b"Kids".to_vec(),
            Object::Array(vec![Object::Reference(ObjectId(999, 0))]),
        );
        objs.insert(ObjectId(1, 0), Object::Dictionary(pages));
        let mut out = Vec::new();
        walk_pages(&objs, ObjectId(1, 0), &mut out, 0).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn page_tree_walk_collects_pages_in_kid_array() {
        // A /Pages node with explicit /Type Pages whose /Kids hold the
        // actual /Page leaves. Exercises the recursive branch of walk_pages.
        let mut objs = HashMap::new();
        let mut leaf = Dictionary::new();
        leaf.insert(b"Type".to_vec(), Object::Name(b"Page".to_vec()));
        objs.insert(ObjectId(3, 0), Object::Dictionary(leaf));
        let mut inner = Dictionary::new();
        inner.insert(b"Type".to_vec(), Object::Name(b"Pages".to_vec()));
        inner.insert(
            b"Kids".to_vec(),
            Object::Array(vec![Object::Reference(ObjectId(3, 0))]),
        );
        objs.insert(ObjectId(2, 0), Object::Dictionary(inner));
        let mut root = Dictionary::new();
        root.insert(b"Type".to_vec(), Object::Name(b"Pages".to_vec()));
        root.insert(
            b"Kids".to_vec(),
            Object::Array(vec![Object::Reference(ObjectId(2, 0))]),
        );
        objs.insert(ObjectId(1, 0), Object::Dictionary(root));
        let mut out = Vec::new();
        walk_pages(&objs, ObjectId(1, 0), &mut out, 0).unwrap();
        assert_eq!(out, vec![ObjectId(3, 0)]);
    }

    #[test]
    fn parse_object_stream_skips_entries_with_bad_offsets() {
        // Header points obj 2 way past the end of the body. Both entries
        // fail the start/end bounds check and produce no output, but the
        // skip-entry branch still gets executed.
        let mut dict = Dictionary::new();
        dict.insert(b"N".to_vec(), Object::Integer(2));
        dict.insert(b"First".to_vec(), Object::Integer(10));
        let body = b"1 0 2 999 hi";
        let entries = parse_object_stream(&dict, body).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn decode_filters_propagates_inflate_error() {
        // FlateDecode stream whose body is garbage.
        let mut dict = Dictionary::new();
        dict.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
        let stream = Stream {
            dict,
            content: b"garbage".to_vec(),
        };
        assert!(decode_filters(&stream).is_err());
    }

    #[test]
    fn parse_at_returns_none_for_mismatched_id_or_bad_offset() {
        let bytes = b"5 0 obj 42 endobj";
        // Asking for id 7 at offset 0 returns None because id 5 lives there.
        assert!(parse_at(bytes, 0, ObjectId(7, 0)).is_none());
        // A wildly out-of-bounds offset also returns None (parse_indirect
        // bails on an empty slice).
        assert!(parse_at(bytes, bytes.len() + 100, ObjectId(5, 0)).is_none());
    }

    #[test]
    fn read_uint_errors_on_overflow() {
        let bytes = b"9999999999999";
        let mut pos = 0;
        assert!(read_uint(bytes, &mut pos).is_err());
    }

    #[test]
    fn read_xref_chain_breaks_on_repeated_offset() {
        // Build a PDF whose /Prev points back at itself — the chain walker
        // must terminate via the visited-set check rather than loop.
        let body = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
";
        let mut out = body.to_vec();
        let xref_offset = out.len();
        let offsets: Vec<usize> = (1..=3)
            .map(|n| {
                let needle = format!("{n} 0 obj");
                find_subslice(&out, needle.as_bytes()).unwrap()
            })
            .collect();
        let mut xref = String::from("xref\n0 4\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str(&format!(
            "trailer <</Size 4/Root 1 0 R/Prev {xref_offset}>>\nstartxref\n{xref_offset}\n%%EOF\n"
        ));
        out.extend_from_slice(xref.as_bytes());
        let doc = Document::load(&out).unwrap();
        assert_eq!(doc.pages().len(), 1);
    }
}
