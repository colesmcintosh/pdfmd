use std::collections::HashMap;

use super::object::{Dictionary, Object, ObjectId};
use super::PdfError;

pub(super) fn collect_pages(
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

pub(super) fn walk_pages(
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
