//! Tagged-PDF structure tree → `(page index, MCID) → Role`.

use std::collections::{HashMap, HashSet};

use crate::pdf::{Dictionary, Document, Object, ObjectId};

const MAX_STRUCT_NODES: usize = 65_536;
const MAX_STRUCT_DEPTH: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Heading(u8),
    Table,
    Tr,
    Th,
    Td,
    List,
    ListItem,
}

pub type RoleMap = HashMap<(usize, u32), Role>;

/// Walk `/StructTreeRoot` and map marked-content IDs to their nearest role.
pub fn role_map(doc: &Document<'_>) -> RoleMap {
    let mut out = RoleMap::new();
    let Some(catalog) = doc.catalog() else {
        return out;
    };
    let Some(root) = catalog.get(b"StructTreeRoot") else {
        return out;
    };
    let page_index: HashMap<ObjectId, usize> = doc
        .pages()
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();
    let mut visited = HashSet::new();
    let mut walk = Walk {
        doc,
        page_index: &page_index,
        out: &mut out,
        visited: &mut visited,
    };
    walk_kid(&mut walk, root, None, None, 0);
    out
}

struct Walk<'a, 'b> {
    doc: &'a Document<'a>,
    page_index: &'a HashMap<ObjectId, usize>,
    out: &'b mut RoleMap,
    visited: &'b mut HashSet<ObjectId>,
}

fn walk_kid(
    walk: &mut Walk<'_, '_>,
    obj: &Object,
    page: Option<usize>,
    role: Option<Role>,
    depth: u32,
) {
    if depth > MAX_STRUCT_DEPTH || walk.visited.len() > MAX_STRUCT_NODES {
        return;
    }
    let resolved = match obj {
        Object::Reference(id) => {
            if !walk.visited.insert(*id) {
                return;
            }
            match walk.doc.get_object(*id) {
                Some(o) => o,
                None => return,
            }
        }
        other => other,
    };
    match resolved {
        Object::Integer(n) if *n >= 0 => {
            if let (Some(p), Some(r)) = (page, role) {
                walk.out.entry((p, *n as u32)).or_insert(r);
            }
        }
        Object::Array(items) => {
            for item in items {
                walk_kid(walk, item, page, role, depth.saturating_add(1));
            }
        }
        Object::Dictionary(dict) => {
            if dict.get(b"Type").and_then(Object::as_name) == Some(b"MCR".as_slice()) {
                let page = dict
                    .get(b"Pg")
                    .and_then(Object::as_reference)
                    .and_then(|id| walk.page_index.get(&id).copied())
                    .or(page);
                let mcid = dict.get(b"MCID").and_then(Object::as_integer);
                if let (Some(p), Some(mcid), Some(r)) = (page, mcid, role) {
                    if mcid >= 0 {
                        walk.out.entry((p, mcid as u32)).or_insert(r);
                    }
                }
                return;
            }
            walk_elem(walk, dict, page, role, depth.saturating_add(1));
        }
        _ => {}
    }
}

fn walk_elem(
    walk: &mut Walk<'_, '_>,
    dict: &Dictionary,
    page: Option<usize>,
    parent_role: Option<Role>,
    depth: u32,
) {
    if depth > MAX_STRUCT_DEPTH || walk.visited.len() > MAX_STRUCT_NODES {
        return;
    }
    let page = dict
        .get(b"Pg")
        .and_then(Object::as_reference)
        .and_then(|id| walk.page_index.get(&id).copied())
        .or(page);
    let role = dict
        .get(b"S")
        .and_then(Object::as_name)
        .and_then(parse_role)
        .or(parent_role);
    let Some(kids) = dict.get(b"K") else {
        return;
    };
    walk_kid(walk, kids, page, role, depth.saturating_add(1));
}

fn parse_role(name: &[u8]) -> Option<Role> {
    Some(match name {
        b"H1" => Role::Heading(1),
        b"H2" => Role::Heading(2),
        b"H3" => Role::Heading(3),
        b"H4" => Role::Heading(4),
        b"H5" => Role::Heading(5),
        b"H6" => Role::Heading(6),
        b"H" | b"Title" => Role::Heading(1),
        b"Table" => Role::Table,
        b"TR" => Role::Tr,
        b"TH" => Role::Th,
        b"TD" => Role::Td,
        b"L" => Role::List,
        b"LI" => Role::ListItem,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::Document;

    fn build_xref_pdf(body: &[u8]) -> Vec<u8> {
        crate::pdf::test_pdf::build_xref_pdf(body)
    }

    #[test]
    fn empty_without_struct_tree() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        assert!(role_map(&doc).is_empty());
    }

    #[test]
    fn maps_heading_mcid_and_mcr_table_cell() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R/StructTreeRoot 5 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
5 0 obj <</Type/StructTreeRoot/K[6 0 R 7 0 R]>> endobj
6 0 obj <</Type/StructElem/S/H1/Pg 3 0 R/K 0>> endobj
7 0 obj <</Type/StructElem/S/Table/K 8 0 R>> endobj
8 0 obj <</Type/StructElem/S/TD/K<</Type/MCR/Pg 3 0 R/MCID 2>>>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let map = role_map(&doc);
        assert_eq!(map.get(&(0, 0)), Some(&Role::Heading(1)));
        assert_eq!(map.get(&(0, 2)), Some(&Role::Td));
    }

    #[test]
    fn ignores_cycles_and_unknown_roles() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R/StructTreeRoot 5 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
5 0 obj <</Type/StructTreeRoot/K 6 0 R>> endobj
6 0 obj <</Type/StructElem/S/P/K[6 0 R 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        assert!(role_map(&doc).is_empty());
    }

    #[test]
    fn parse_role_covers_standard_names() {
        assert_eq!(parse_role(b"H2"), Some(Role::Heading(2)));
        assert_eq!(parse_role(b"H3"), Some(Role::Heading(3)));
        assert_eq!(parse_role(b"H4"), Some(Role::Heading(4)));
        assert_eq!(parse_role(b"H5"), Some(Role::Heading(5)));
        assert_eq!(parse_role(b"H6"), Some(Role::Heading(6)));
        assert_eq!(parse_role(b"H"), Some(Role::Heading(1)));
        assert_eq!(parse_role(b"Title"), Some(Role::Heading(1)));
        assert_eq!(parse_role(b"Table"), Some(Role::Table));
        assert_eq!(parse_role(b"TR"), Some(Role::Tr));
        assert_eq!(parse_role(b"TH"), Some(Role::Th));
        assert_eq!(parse_role(b"L"), Some(Role::List));
        assert_eq!(parse_role(b"LI"), Some(Role::ListItem));
        assert_eq!(parse_role(b"P"), None);
    }
}
