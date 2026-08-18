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

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: &[(&[u8], Object)]) -> Object {
        let mut d = Dictionary::new();
        for (key, value) in entries {
            d.insert(key.to_vec(), value.clone());
        }
        Object::Dictionary(d)
    }

    fn node(type_: &[u8], kids: &[u32]) -> Object {
        let refs = kids
            .iter()
            .map(|n| Object::Reference(ObjectId(*n, 0)))
            .collect();
        dict(&[
            (b"Type", Object::Name(type_.to_vec())),
            (b"Kids", Object::Array(refs)),
        ])
    }

    fn walk(objects: &[(u32, Object)], root: u32) -> Result<Vec<ObjectId>, PdfError> {
        let map: HashMap<ObjectId, Object> = objects
            .iter()
            .map(|(n, obj)| (ObjectId(*n, 0), obj.clone()))
            .collect();
        let mut out = Vec::new();
        walk_pages(&map, ObjectId(root, 0), &mut out, 0)?;
        Ok(out)
    }

    #[test]
    fn walk_pages_rejects_excessive_depth() {
        // A self-referential /Pages cycle must bail out via the depth guard.
        let objects: HashMap<ObjectId, Object> = [(ObjectId(1, 0), node(b"Pages", &[1]))]
            .into_iter()
            .collect();
        let mut out = Vec::new();
        assert!(walk_pages(&objects, ObjectId(1, 0), &mut out, 65).is_err());
    }

    #[test]
    fn walk_pages_returns_empty_for_nodes_without_reachable_leaves() {
        // Missing node, /Pages without /Kids, /Kids naming a missing node,
        // and non-reference /Kids entries are all silent no-ops.
        assert!(walk(&[], 99).unwrap().is_empty());
        let pages_only = dict(&[(b"Type", Object::Name(b"Pages".to_vec()))]);
        assert!(walk(&[(1, pages_only)], 1).unwrap().is_empty());
        assert!(walk(&[(1, node(b"Pages", &[999]))], 1).unwrap().is_empty());
        let bad_kids = dict(&[
            (b"Type", Object::Name(b"Pages".to_vec())),
            (b"Kids", Object::Array(vec![Object::Integer(42)])),
        ]);
        assert!(walk(&[(1, bad_kids)], 1).unwrap().is_empty());
    }

    #[test]
    fn walk_pages_pushes_leaf_when_called_with_type_page_directly() {
        // A /Pages reference pointing at a /Type Page leaf is unusual but
        // valid; it should be pushed without recursing.
        let leaf = dict(&[(b"Type", Object::Name(b"Page".to_vec()))]);
        assert_eq!(walk(&[(1, leaf)], 1).unwrap(), vec![ObjectId(1, 0)]);
    }

    #[test]
    fn walk_pages_collects_pages_in_nested_kid_arrays() {
        let leaf = dict(&[(b"Type", Object::Name(b"Page".to_vec()))]);
        let objects = [
            (1, node(b"Pages", &[2])),
            (2, node(b"Pages", &[3])),
            (3, leaf),
        ];
        assert_eq!(walk(&objects, 1).unwrap(), vec![ObjectId(3, 0)]);
    }

    #[test]
    fn collect_pages_errors_when_the_catalog_chain_is_broken() {
        let root = (b"Root".as_slice(), Object::Reference(ObjectId(1, 0)));
        let catalog = dict(&[(b"Type", Object::Name(b"Catalog".to_vec()))]);
        for (objects, trailer) in [
            (Vec::new(), Vec::new()),            // no /Root
            (Vec::new(), vec![root.clone()]),    // /Root names a missing object
            (vec![(1u32, catalog)], vec![root]), // catalog without /Pages
        ] {
            let map: HashMap<ObjectId, Object> = objects
                .into_iter()
                .map(|(n, obj)| (ObjectId(n, 0), obj))
                .collect();
            let Object::Dictionary(trailer) = dict(&trailer) else {
                unreachable!()
            };
            assert!(collect_pages(&map, &trailer).is_err());
        }
    }
}
