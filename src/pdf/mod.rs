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
    Truncated,
    BadXref(String),
    BadObject(String),
    BadFilter(String),
    Deflate(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::NotPdf => f.write_str("input does not look like a PDF"),
            PdfError::Truncated => f.write_str("PDF truncated"),
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
    let s = std::str::from_utf8(&tail[n_start..i])
        .map_err(|_| PdfError::BadXref("bad startxref offset".into()))?;
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
    Ok((
        entries,
        final_trailer.ok_or_else(|| PdfError::BadXref("no trailer".into()))?,
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
            let offset_s = std::str::from_utf8(&row[0..10])
                .map_err(|_| PdfError::BadXref("non-utf8 xref offset".into()))?;
            let gen_s = std::str::from_utf8(&row[11..16])
                .map_err(|_| PdfError::BadXref("non-utf8 xref gen".into()))?;
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
    let n = dict
        .get(b"N")
        .and_then(Object::as_integer)
        .ok_or_else(|| PdfError::BadObject("objstm missing /N".into()))? as usize;
    let first =
        dict.get(b"First")
            .and_then(Object::as_integer)
            .ok_or_else(|| PdfError::BadObject("objstm missing /First".into()))? as usize;

    // Header: N pairs of "obj_num offset" pointing into the body at byte
    // /First. The Nth object ends at the next offset (or end of stream).
    let mut p = Parser::with_pos(decoded, 0);
    let mut headers: Vec<(u32, usize)> = Vec::with_capacity(n);
    for _ in 0..n {
        p.skip_ws_and_comments();
        let num = read_uint(decoded, &mut p.pos)? as u32;
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
    let s = std::str::from_utf8(&bytes[start..*pos])
        .map_err(|_| PdfError::BadXref("non-utf8 integer".into()))?;
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
