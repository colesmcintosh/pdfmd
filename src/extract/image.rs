//! Image XObject discovery and pass-through extraction.
//!
//! PDF embeds images as Stream objects whose `/Filter` matches the wire
//! format of a common image type. For `DCTDecode` (JPEG) and `JPXDecode`
//! (JPEG 2000), the stream bytes are already a valid image file, so we
//! can write them straight to disk without decoding.

use std::collections::HashMap;

use crate::pdf::{Dictionary, Document, Object, ObjectId};

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
pub fn page_xobject_refs(doc: &Document, resources: &Dictionary) -> HashMap<Vec<u8>, ObjectId> {
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

/// If the object is an image XObject in a pass-through filter, return its
/// file extension and the raw stream bytes. Form XObjects, inline-decoded
/// bitmaps, and unsupported filter chains return `None`.
pub fn extract_image(doc: &Document, obj_id: ObjectId) -> Option<(&'static str, Vec<u8>)> {
    let stream = doc.get_object(obj_id)?.as_stream()?;
    let dict = &stream.dict;

    let subtype = dict.get(b"Subtype")?.as_name_str()?;
    if subtype != "Image" {
        return None;
    }

    let filter = dict.get(b"Filter")?;
    let filter_name = match filter {
        Object::Name(n) => std::str::from_utf8(n).ok()?,
        // Multi-filter chains (e.g. /Filter [/ASCII85Decode /DCTDecode])
        // would need us to apply every filter except the last before
        // writing the bytes. The pass-through case — a single-element
        // array — is common enough to handle, but we leave true chains
        // for a future pass.
        Object::Array(arr) if arr.len() == 1 => arr.first()?.as_name_str()?,
        _ => return None,
    };

    let ext = match filter_name {
        "DCTDecode" => "jpg",
        "JPXDecode" => "jp2",
        _ => return None,
    };

    Some((ext, stream.content.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::Document;

    /// Build and load a PDF whose extra indirect objects come from `defs`.
    fn build_doc(defs: &[(u32, &str)]) -> Document {
        let mut body = String::from("%PDF-1.4\n");
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        for (n, raw) in defs {
            body.push_str(&format!("{n} 0 obj {raw} endobj\n"));
        }
        let xref_offset = body.len();
        let max = defs.iter().map(|(n, _)| *n).max().unwrap_or(3).max(3);
        let mut xref = String::from("xref\n");
        xref.push_str(&format!("0 {}\n", max + 1));
        xref.push_str("0000000000 65535 f \n");
        for n in 1..=max {
            let needle = format!("{n} 0 obj");
            match (0..=body.len() - needle.len())
                .find(|&i| body.as_bytes()[i..i + needle.len()] == *needle.as_bytes())
            {
                Some(off) => xref.push_str(&format!("{off:010} 00000 n \n")),
                None => xref.push_str("0000000000 00000 f \n"),
            }
        }
        xref.push_str(&format!(
            "trailer <</Size {}/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n",
            max + 1
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(xref.as_bytes());
        Document::load(&bytes).expect("load")
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
    fn extract_image_rejects_multi_element_filter_chain() {
        let doc = build_doc(&[(
            7,
            "<</Subtype/Image/Filter [/ASCII85Decode /DCTDecode]/Length 0>>\nstream\n\nendstream",
        )]);
        assert!(extract_image(&doc, ObjectId(7, 0)).is_none());
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
