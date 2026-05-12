//! Image XObject discovery and pass-through extraction.
//!
//! PDF embeds images as Stream objects whose `/Filter` matches the wire
//! format of a common image type. For `DCTDecode` (JPEG) and `JPXDecode`
//! (JPEG 2000), the stream bytes are already a valid image file, so we
//! can write them straight to disk without decoding.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object, ObjectId};

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
    let Ok(xobj_obj) = resources.get(b"XObject") else {
        return out;
    };
    let xobj_dict = match xobj_obj {
        Object::Reference(id) => doc.get_object(*id).and_then(Object::as_dict).ok(),
        Object::Dictionary(d) => Some(d),
        _ => None,
    };
    let Some(xobj_dict) = xobj_dict else {
        return out;
    };
    for (name, obj) in xobj_dict.iter() {
        if let Object::Reference(id) = obj {
            out.insert(name.clone(), *id);
        }
    }
    out
}

/// If the object is an image XObject in a pass-through filter, return its
/// file extension and the raw stream bytes. Form XObjects, inline-decoded
/// bitmaps, and unsupported filter chains return `None`.
pub fn extract_image(doc: &Document, obj_id: ObjectId) -> Option<(&'static str, Vec<u8>)> {
    let stream = doc.get_object(obj_id).ok()?.as_stream().ok()?;
    let dict = &stream.dict;

    let subtype = dict.get(b"Subtype").ok()?.as_name_str().ok()?;
    if subtype != "Image" {
        return None;
    }

    let filter = dict.get(b"Filter").ok()?;
    let filter_name = match filter {
        Object::Name(n) => std::str::from_utf8(n).ok()?,
        // Multi-filter chains (e.g. /Filter [/ASCII85Decode /DCTDecode])
        // would need us to apply every filter except the last before
        // writing the bytes. The pass-through case — a single-element
        // array — is common enough to handle, but we leave true chains
        // for a future pass.
        Object::Array(arr) if arr.len() == 1 => arr.first()?.as_name_str().ok()?,
        _ => return None,
    };

    let ext = match filter_name {
        "DCTDecode" => "jpg",
        "JPXDecode" => "jp2",
        _ => return None,
    };

    Some((ext, stream.content.clone()))
}
