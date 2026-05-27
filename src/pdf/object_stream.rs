use super::object::{Dictionary, Object};
use super::parser::Parser;
use super::xref::read_uint;
use super::PdfError;

pub(super) fn parse_object_stream(
    dict: &Dictionary,
    decoded: &[u8],
) -> Result<Vec<(u32, Vec<u8>)>, PdfError> {
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
