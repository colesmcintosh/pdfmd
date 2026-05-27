//! Image XObject discovery and extraction.
//!
//! PDF embeds images as Stream objects. Some filters already match a common
//! file format (`DCTDecode` = JPEG, `JPXDecode` = JPEG 2000), so those bytes
//! can be written directly. Flate/ASCII decoded 8-bit DeviceGray, DeviceRGB,
//! and DeviceCMYK rasters are converted into PNG with a tiny in-crate encoder.

use std::collections::HashMap;

use crate::pdf::{Dictionary, Document, Object, ObjectId};

mod png;

use png::encode_png;
#[cfg(test)]
use png::zlib_stored;

/// An image extracted from the PDF, ready to be written to disk.
pub struct ExtractedImage {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// Map from a page's XObject-resource name (e.g. `b"Im1"`) to the filename
/// chosen for the extracted image. Names absent from this map either point
/// at a Form XObject or an image in a filter we don't pass through.
pub type PageImages<'a> = HashMap<Vec<u8>, &'a str>;

/// Walk a page's `/Resources/XObject` dictionary and collect
/// `name → ObjectId` entries, mirroring `page_font_refs`.
pub fn page_xobject_refs(doc: &Document<'_>, resources: &Dictionary) -> HashMap<Vec<u8>, ObjectId> {
    let mut out = HashMap::new();
    let Some(xobj_obj) = resources.get(b"XObject") else {
        return out;
    };
    let xobj_dict = match xobj_obj {
        Object::Reference(id) => doc.get_object(*id).and_then(Object::as_dict),
        Object::Dictionary(d) => Some(d),
        _ => None,
    };
    let Some(xobj_dict) = xobj_dict else {
        return out;
    };
    for (name, obj) in xobj_dict.iter() {
        if let Some(id) = obj.as_reference() {
            out.insert(name.to_vec(), id);
        }
    }
    out
}

/// If the object is an image XObject in a supported representation, return its
/// file extension and encoded bytes. Form XObjects, unsupported colour spaces,
/// and bit depths other than 8 return `None`.
pub fn extract_image(doc: &Document<'_>, obj_id: ObjectId) -> Option<(&'static str, Vec<u8>)> {
    let stream = doc.get_object(obj_id)?.as_stream()?;
    let dict = &stream.dict;

    let subtype = dict.get(b"Subtype")?.as_name_str()?;
    if subtype != "Image" {
        return None;
    }

    let filters = filter_names(dict);
    if let Some(last) = filters.last().copied() {
        if last == "DCTDecode" || last == "DCT" {
            let bytes = if filters.len() == 1 {
                doc.stream_content(stream).to_vec()
            } else {
                doc.decode_stream(stream).ok()?
            };
            return Some(("jpg", bytes));
        }
        if last == "JPXDecode" {
            let bytes = if filters.len() == 1 {
                doc.stream_content(stream).to_vec()
            } else {
                doc.decode_stream(stream).ok()?
            };
            return Some(("jp2", bytes));
        }
    }

    let pixels = doc.decode_stream(stream).ok()?;
    let png = encode_raster_png(dict, &pixels)?;
    Some(("png", png))
}

fn filter_names(dict: &Dictionary) -> Vec<&str> {
    match dict.get(b"Filter") {
        Some(Object::Name(n)) => std::str::from_utf8(n).map(|s| vec![s]).unwrap_or_default(),
        Some(Object::Array(arr)) => arr.iter().filter_map(Object::as_name_str).collect(),
        _ => Vec::new(),
    }
}

fn encode_raster_png(dict: &Dictionary, data: &[u8]) -> Option<Vec<u8>> {
    let width = image_dim(dict, b"Width", b"W")?;
    let height = image_dim(dict, b"Height", b"H")?;
    if width > u32::MAX as usize || height > u32::MAX as usize {
        return None;
    }
    let bpc = dict
        .get(b"BitsPerComponent")
        .or_else(|| dict.get(b"BPC"))
        .and_then(Object::as_integer)
        .unwrap_or(8);
    if bpc != 8 {
        return None;
    }

    let color_space = color_space(dict)?;
    let components = color_space.components();
    let expected = width.checked_mul(height)?.checked_mul(components)?;
    if data.len() < expected {
        return None;
    }

    match color_space {
        ColorSpace::Gray => Some(encode_png(width, height, 0, &data[..expected])),
        ColorSpace::Rgb => Some(encode_png(width, height, 2, &data[..expected])),
        ColorSpace::Cmyk => Some(encode_png(
            width,
            height,
            2,
            &cmyk_to_rgb(&data[..expected])?,
        )),
    }
}

fn image_dim(dict: &Dictionary, key: &[u8], short: &[u8]) -> Option<usize> {
    let n = dict
        .get(key)
        .or_else(|| dict.get(short))
        .and_then(Object::as_integer)?;
    usize::try_from(n).ok().filter(|n| *n > 0)
}

#[derive(Clone, Copy)]
enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
}

impl ColorSpace {
    fn components(self) -> usize {
        match self {
            ColorSpace::Gray => 1,
            ColorSpace::Rgb => 3,
            ColorSpace::Cmyk => 4,
        }
    }
}

fn color_space(dict: &Dictionary) -> Option<ColorSpace> {
    let obj = dict.get(b"ColorSpace").or_else(|| dict.get(b"CS"))?;
    let name = match obj {
        Object::Name(n) => std::str::from_utf8(n).ok()?,
        Object::Array(arr) => arr.first()?.as_name_str()?,
        _ => return None,
    };
    match name {
        "DeviceGray" | "G" => Some(ColorSpace::Gray),
        "DeviceRGB" | "RGB" => Some(ColorSpace::Rgb),
        "DeviceCMYK" | "CMYK" => Some(ColorSpace::Cmyk),
        _ => None,
    }
}

fn cmyk_to_rgb(data: &[u8]) -> Option<Vec<u8>> {
    let pixels = data.len().checked_div(4)?;
    let mut out = Vec::with_capacity(pixels.checked_mul(3)?);
    for cmyk in data.chunks_exact(4) {
        let c = cmyk[0] as u16;
        let m = cmyk[1] as u16;
        let y = cmyk[2] as u16;
        let k = cmyk[3] as u16;
        out.push((255u16.saturating_sub((c + k).min(255))) as u8);
        out.push((255u16.saturating_sub((m + k).min(255))) as u8);
        out.push((255u16.saturating_sub((y + k).min(255))) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::Document;

    /// Build and load a PDF whose extra indirect objects come from `defs`.
    fn build_doc(defs: &[(u32, &str)]) -> Document<'static> {
        let mut body = String::from("%PDF-1.4\n");
        let defs: Vec<(u32, &[u8])> = defs.iter().map(|(n, raw)| (*n, raw.as_bytes())).collect();
        build_doc_from_body(&mut body, &defs)
    }

    fn build_doc_bytes(defs: &[(u32, &[u8])]) -> Document<'static> {
        let mut body = String::from("%PDF-1.4\n");
        build_doc_from_body(&mut body, defs)
    }

    fn build_doc_from_body(body: &mut String, defs: &[(u32, &[u8])]) -> Document<'static> {
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        let mut body = body.as_bytes().to_vec();
        for (n, raw) in defs {
            body.extend_from_slice(format!("{n} 0 obj ").as_bytes());
            body.extend_from_slice(raw);
            body.extend_from_slice(b" endobj\n");
        }
        let xref_offset = body.len();
        let max = defs.iter().map(|(n, _)| *n).max().unwrap_or(3).max(3);
        let mut xref = String::from("xref\n");
        xref.push_str(&format!("0 {}\n", max + 1));
        xref.push_str("0000000000 65535 f \n");
        for n in 1..=max {
            let needle = format!("{n} 0 obj");
            match (0..=body.len() - needle.len())
                .find(|&i| body[i..i + needle.len()] == *needle.as_bytes())
            {
                Some(off) => xref.push_str(&format!("{off:010} 00000 n \n")),
                None => xref.push_str("0000000000 00000 f \n"),
            }
        }
        xref.push_str(&format!(
            "trailer <</Size {}/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n",
            max + 1
        ));
        body.extend_from_slice(xref.as_bytes());
        let bytes = Box::leak(body.into_boxed_slice());
        Document::load(bytes).expect("load")
    }

    #[test]
    fn page_xobject_refs_handles_direct_dict() {
        let mut res = Dictionary::new();
        let mut xobj = Dictionary::new();
        xobj.insert(b"Im1".to_vec(), Object::Reference(ObjectId(7, 0)));
        res.insert(b"XObject".to_vec(), Object::Dictionary(xobj));
        let doc = build_doc(&[]);
        let refs = page_xobject_refs(&doc, &res);
        assert_eq!(refs.get(b"Im1".as_slice()), Some(&ObjectId(7, 0)));
    }

    #[test]
    fn page_xobject_refs_handles_indirect_dict() {
        let doc = build_doc(&[(4, "<</Im1 7 0 R>>")]);
        let mut res = Dictionary::new();
        res.insert(b"XObject".to_vec(), Object::Reference(ObjectId(4, 0)));
        let refs = page_xobject_refs(&doc, &res);
        assert_eq!(refs.get(b"Im1".as_slice()), Some(&ObjectId(7, 0)));
    }

    #[test]
    fn page_xobject_refs_returns_empty_when_missing_or_wrong_shape() {
        let doc = build_doc(&[]);
        assert!(page_xobject_refs(&doc, &Dictionary::new()).is_empty());
        let mut res = Dictionary::new();
        res.insert(b"XObject".to_vec(), Object::Integer(0));
        assert!(page_xobject_refs(&doc, &res).is_empty());
        // Reference to a non-dict object also yields empty.
        let mut res = Dictionary::new();
        res.insert(b"XObject".to_vec(), Object::Reference(ObjectId(999, 0)));
        assert!(page_xobject_refs(&doc, &res).is_empty());
    }

    #[test]
    fn extract_image_passes_through_jpeg() {
        // 5-byte stream pretending to be JPEG bytes.
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Filter/DCTDecode/Length 5>>\nstream\nHELLO\nendstream",
        )]);
        let (ext, bytes) = extract_image(&doc, ObjectId(7, 0)).unwrap();
        assert_eq!(ext, "jpg");
        assert_eq!(bytes, b"HELLO");
    }

    #[test]
    fn extract_image_passes_through_jpx_in_array_filter() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Filter [/JPXDecode]/Length 5>>\nstream\nHELLO\nendstream",
        )]);
        let (ext, _bytes) = extract_image(&doc, ObjectId(7, 0)).unwrap();
        assert_eq!(ext, "jp2");
    }

    #[test]
    fn extract_image_rejects_form_xobject() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Form/Filter/DCTDecode/Length 0>>\nstream\n\nendstream",
        )]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());
    }

    #[test]
    fn extract_image_rejects_unsupported_filter() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Filter/FlateDecode/Length 0>>\nstream\n\nendstream",
        )]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());
    }

    #[test]
    fn extract_image_converts_raw_rgb_raster_to_png() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Width 2/Height 1/ColorSpace/DeviceRGB/BitsPerComponent 8/Length 6>>\nstream\nABCDEF\nendstream",
        )]);
        let (ext, bytes) = extract_image(&doc, ObjectId(7, 0)).unwrap();
        assert_eq!(ext, "png");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(bytes.windows(4).any(|w| w == b"IHDR"));
        assert!(bytes.windows(4).any(|w| w == b"IDAT"));
    }

    #[test]
    fn extract_image_converts_flate_gray_raster_to_png() {
        let pixels = b"ABCD";
        let zlib = zlib_stored(pixels);
        let mut obj = format!(
            "<</Subtype/Image/Width 2/Height 2/ColorSpace/DeviceGray/BitsPerComponent 8/Filter/FlateDecode/Length {}>>\nstream\n",
            zlib.len()
        )
        .into_bytes();
        obj.extend_from_slice(&zlib);
        obj.extend_from_slice(b"\nendstream");
        let doc = build_doc_bytes(&[(7, &obj)]);
        let (ext, bytes) = extract_image(&doc, ObjectId(7, 0)).unwrap();
        assert_eq!(ext, "png");
        // IHDR color type byte: signature(8) + len(4) + type(4) + IHDR data offset 9.
        assert_eq!(bytes[25], 0);
    }

    #[test]
    fn extract_image_decodes_filter_chain_before_jpeg_passthrough() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Filter [/ASCIIHexDecode /DCTDecode]/Length 5>>\nstream\n4869>\nendstream",
        )]);
        let (ext, bytes) = extract_image(&doc, ObjectId(7, 0)).unwrap();
        assert_eq!(ext, "jpg");
        assert_eq!(bytes, b"Hi");
    }

    #[test]
    fn extract_image_converts_cmyk_raster_to_rgb_png() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/W 1/H 1/CS/DeviceCMYK/BPC 8/Length 4>>\nstream\nAAAA\nendstream",
        )]);
        let (ext, bytes) = extract_image(&doc, ObjectId(7, 0)).unwrap();
        assert_eq!(ext, "png");
        assert_eq!(bytes[25], 2);
    }

    #[test]
    fn extract_image_rejects_unsupported_raster_shapes() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Width 1/Height 1/ColorSpace/DeviceRGB/BitsPerComponent 1/Length 1>>\nstream\nA\nendstream",
        )]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());

        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Width 1/Height 1/ColorSpace/Indexed/BitsPerComponent 8/Length 1>>\nstream\nA\nendstream",
        )]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());
    }

    #[test]
    fn extract_image_rejects_invalid_raster_metadata() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Width 2/Height 1/ColorSpace/DeviceRGB/BitsPerComponent 8/Length 3>>\nstream\nABC\nendstream",
        )]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());

        let mut dict = Dictionary::new();
        dict.insert(b"Width".to_vec(), Object::Integer(u32::MAX as i64 + 1));
        dict.insert(b"Height".to_vec(), Object::Integer(1));
        dict.insert(b"ColorSpace".to_vec(), Object::Name(b"DeviceGray".to_vec()));
        assert!(encode_raster_png(&dict, &[]).is_none());

        let mut dict = Dictionary::new();
        dict.insert(b"Width".to_vec(), Object::Integer(1));
        dict.insert(b"Height".to_vec(), Object::Integer(1));
        dict.insert(
            b"ColorSpace".to_vec(),
            Object::Array(vec![Object::Integer(0)]),
        );
        assert!(encode_raster_png(&dict, &[0]).is_none());

        let mut dict = Dictionary::new();
        dict.insert(b"Width".to_vec(), Object::Integer(1));
        dict.insert(b"Height".to_vec(), Object::Integer(1));
        dict.insert(b"ColorSpace".to_vec(), Object::Integer(0));
        assert!(encode_raster_png(&dict, &[0]).is_none());
    }

    #[test]
    fn png_helpers_handle_empty_payloads() {
        let png = encode_png(0, 0, 0, &[]);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(
            zlib_stored(&[]),
            vec![0x78, 0x01, 1, 0, 0, 0xFF, 0xFF, 0, 0, 0, 1]
        );
    }

    #[test]
    fn extract_image_accepts_multi_element_jpx_filter_chain() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Filter [/ASCIIHexDecode /JPXDecode]/Length 5>>\nstream\n4869>\nendstream",
        )]);
        let (ext, bytes) = extract_image(&doc, ObjectId(7, 0)).unwrap();
        assert_eq!(ext, "jp2");
        assert_eq!(bytes, b"Hi");
    }

    #[test]
    fn extract_image_returns_none_for_missing_object() {
        let doc = build_doc(&[]);
        assert!(extract_image(&doc, ObjectId(99, 0)).is_none());
    }

    #[test]
    fn extract_image_returns_none_when_object_is_not_a_stream() {
        // Object exists but is a plain dictionary, not a stream.
        let doc = build_doc(&[(7, "<</Subtype/Image>>")]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());
    }

    #[test]
    fn extract_image_returns_none_without_subtype() {
        // Stream object missing /Subtype — second `?` chain bails.
        let doc = build_doc(&[(7, "<</Filter/DCTDecode/Length 0>>\nstream\n\nendstream")]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());
    }

    #[test]
    fn extract_image_returns_none_without_filter() {
        // Image XObject with no /Filter — the pass-through path requires one.
        let doc = build_doc(&[(7, "<</Subtype/Image/Length 0>>\nstream\n\nendstream")]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());
    }

    #[test]
    fn extract_image_returns_none_when_filter_array_first_is_not_name() {
        // /Filter is a single-element array but the element isn't a Name.
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Filter [42]/Length 0>>\nstream\n\nendstream",
        )]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());
    }
}
