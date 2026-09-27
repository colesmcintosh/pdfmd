use std::collections::{HashMap, HashSet};

use super::object::{Dictionary, Object, ObjectId};

const MAX_DEPTH: u32 = 64;

/// Page ids in reading order. Falls back to every `/Type /Page` object in
/// id order when the catalog or its `/Pages` tree is unusable — common in
/// files that needed xref repair.
pub(super) fn collect_pages(
    objects: &HashMap<ObjectId, Object>,
    trailer: &Dictionary,
) -> Vec<ObjectId> {
    let catalog = trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .and_then(|id| objects.get(&id))
        .and_then(Object::as_dict)
        .filter(|d| d.get(b"Pages").is_some())
        .or_else(|| find_catalog(objects));
    let mut out = Vec::new();
    if let Some(pages_ref) = catalog
        .and_then(|c| c.get(b"Pages"))
        .and_then(Object::as_reference)
    {
        walk_pages(objects, pages_ref, &mut out, &mut HashSet::new(), 0);
    }
    if out.is_empty() {
        out = objects
            .iter()
            .filter(|(_, obj)| is_type(obj.as_dict(), b"Page"))
            .map(|(id, _)| *id)
            .collect();
        out.sort();
    }
    out
}

fn find_catalog(objects: &HashMap<ObjectId, Object>) -> Option<&Dictionary> {
    let mut catalogs: Vec<(&ObjectId, &Dictionary)> = objects
        .iter()
        .filter_map(|(id, obj)| Some((id, obj.as_dict()?)))
        .filter(|(_, d)| is_type(Some(d), b"Catalog") && d.get(b"Pages").is_some())
        .collect();
    // Highest id: incremental updates append newer catalogs.
    catalogs.sort_by_key(|(id, _)| **id);
    catalogs.last().map(|(_, d)| *d)
}

fn is_type(dict: Option<&Dictionary>, type_: &[u8]) -> bool {
    dict.and_then(|d| d.get(b"Type")).and_then(Object::as_name) == Some(type_)
}

/// Depth-first walk. `seen` breaks cycles and repeated subtrees, which
/// damaged or hostile files use to blow up the page count.
pub(super) fn walk_pages(
    objects: &HashMap<ObjectId, Object>,
    node_id: ObjectId,
    out: &mut Vec<ObjectId>,
    seen: &mut HashSet<ObjectId>,
    depth: u32,
) {
    if depth > MAX_DEPTH || !seen.insert(node_id) {
        return;
    }
    let Some(node) = objects.get(&node_id).and_then(Object::as_dict) else {
        return;
    };
    // Decide leaf vs interior by /Type, tolerating a root /Pages node
    // that omits it.
    if is_type(Some(node), b"Page") {
        out.push(node_id);
        return;
    }
    let Some(kids) = node.get(b"Kids").and_then(Object::as_array) else {
        return;
    };
    for kid_id in kids.iter().filter_map(Object::as_reference) {
        walk_pages(objects, kid_id, out, seen, depth + 1);
    }
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

    fn map(objects: Vec<(u32, Object)>) -> HashMap<ObjectId, Object> {
        objects
            .into_iter()
            .map(|(n, obj)| (ObjectId(n, 0), obj))
            .collect()
    }

    fn walk(objects: &[(u32, Object)], root: u32) -> Vec<ObjectId> {
        let mut out = Vec::new();
        let map = map(objects.to_vec());
        walk_pages(&map, ObjectId(root, 0), &mut out, &mut HashSet::new(), 0);
        out
    }

    fn leaf() -> Object {
        dict(&[(b"Type", Object::Name(b"Page".to_vec()))])
    }

    #[test]
    fn walk_pages_survives_cycles_and_depth_limit() {
        assert!(walk(&[(1, node(b"Pages", &[1]))], 1).is_empty());
        let objects = [(1, node(b"Pages", &[2, 2, 1])), (2, leaf())];
        assert_eq!(walk(&objects, 1), vec![ObjectId(2, 0)]);
        let map = map(vec![(1, leaf())]);
        let mut out = Vec::new();
        walk_pages(
            &map,
            ObjectId(1, 0),
            &mut out,
            &mut HashSet::new(),
            MAX_DEPTH + 1,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn walk_pages_returns_empty_for_nodes_without_reachable_leaves() {
        // Missing node, /Pages without /Kids, /Kids naming a missing node,
        // and non-reference /Kids entries are all silent no-ops.
        assert!(walk(&[], 99).is_empty());
        let pages_only = dict(&[(b"Type", Object::Name(b"Pages".to_vec()))]);
        assert!(walk(&[(1, pages_only)], 1).is_empty());
        assert!(walk(&[(1, node(b"Pages", &[999]))], 1).is_empty());
        let bad_kids = dict(&[
            (b"Type", Object::Name(b"Pages".to_vec())),
            (b"Kids", Object::Array(vec![Object::Integer(42)])),
        ]);
        assert!(walk(&[(1, bad_kids)], 1).is_empty());
    }

    #[test]
    fn walk_pages_pushes_leaf_when_called_with_type_page_directly() {
        // A /Pages reference pointing at a /Type Page leaf is unusual but
        // valid; it should be pushed without recursing.
        assert_eq!(walk(&[(1, leaf())], 1), vec![ObjectId(1, 0)]);
    }

    #[test]
    fn walk_pages_collects_pages_in_nested_kid_arrays() {
        let objects = [
            (1, node(b"Pages", &[2])),
            (2, node(b"Pages", &[3])),
            (3, leaf()),
        ];
        assert_eq!(walk(&objects, 1), vec![ObjectId(3, 0)]);
    }

    fn trailer(root: Option<u32>) -> Dictionary {
        let mut d = Dictionary::new();
        if let Some(n) = root {
            d.insert(b"Root".to_vec(), Object::Reference(ObjectId(n, 0)));
        }
        d
    }

    fn catalog(pages: u32) -> Object {
        dict(&[
            (b"Type", Object::Name(b"Catalog".to_vec())),
            (b"Pages", Object::Reference(ObjectId(pages, 0))),
        ])
    }

    #[test]
    fn collect_pages_follows_the_trailer_root() {
        let objects = map(vec![
            (1, catalog(2)),
            (2, node(b"Pages", &[4, 3])),
            (3, leaf()),
            (4, leaf()),
        ]);
        assert_eq!(
            collect_pages(&objects, &trailer(Some(1))),
            vec![ObjectId(4, 0), ObjectId(3, 0)]
        );
    }

    #[test]
    fn collect_pages_finds_the_catalog_without_a_root() {
        let objects = map(vec![
            (1, catalog(3)),
            (2, catalog(4)),
            (3, node(b"Pages", &[5])),
            (4, node(b"Pages", &[6])),
            (5, leaf()),
            (6, leaf()),
        ]);
        // Newest catalog wins.
        assert_eq!(
            collect_pages(&objects, &trailer(None)),
            vec![ObjectId(6, 0)]
        );
        assert_eq!(
            collect_pages(&objects, &trailer(Some(99))),
            vec![ObjectId(6, 0)]
        );
    }

    #[test]
    fn collect_pages_falls_back_to_orphan_pages_in_id_order() {
        let bare_catalog = dict(&[(b"Type", Object::Name(b"Catalog".to_vec()))]);
        let objects = map(vec![(1, bare_catalog), (7, leaf()), (3, leaf())]);
        assert_eq!(
            collect_pages(&objects, &trailer(Some(1))),
            vec![ObjectId(3, 0), ObjectId(7, 0)]
        );
        assert!(collect_pages(&HashMap::new(), &trailer(None)).is_empty());
    }
}
