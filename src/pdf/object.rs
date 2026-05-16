//! PDF object model: just the variants the text extractor cares about.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub u32, pub u16);

#[derive(Debug, Clone)]
pub enum Object {
    Null,
    // Boolean and String are parsed for completeness; the text extractor
    // doesn't read them but a corrupt dict with such a value should still
    // round-trip rather than fail.
    Boolean(#[allow(dead_code)] bool),
    Integer(i64),
    Real(f32),
    String(#[allow(dead_code)] Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjectId),
}

impl Object {
    pub fn as_dict(&self) -> Option<&Dictionary> {
        if let Object::Dictionary(d) = self {
            Some(d)
        } else {
            None
        }
    }
    pub fn as_array(&self) -> Option<&[Object]> {
        if let Object::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }
    pub fn as_name(&self) -> Option<&[u8]> {
        if let Object::Name(n) = self {
            Some(n)
        } else {
            None
        }
    }
    pub fn as_name_str(&self) -> Option<&str> {
        self.as_name().and_then(|n| std::str::from_utf8(n).ok())
    }
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Object::Integer(i) => Some(*i),
            Object::Real(r) => Some(*r as i64),
            _ => None,
        }
    }
    pub fn as_stream(&self) -> Option<&Stream> {
        if let Object::Stream(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_reference(&self) -> Option<ObjectId> {
        if let Object::Reference(id) = self {
            Some(*id)
        } else {
            None
        }
    }
}

/// Small ordered map. PDF dicts are tiny (rarely more than ~10 entries) so
/// linear lookup beats hashing every key.
#[derive(Debug, Default, Clone)]
pub struct Dictionary {
    entries: Vec<(Vec<u8>, Object)>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, key: Vec<u8>, value: Object) {
        // Replace existing entry by the same key so updates win over inserts,
        // matching the dictionary-as-map intent in the PDF spec.
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }
    pub fn get(&self, key: &[u8]) -> Option<&Object> {
        self.entries
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &Object)> {
        self.entries.iter().map(|(k, v)| (k.as_slice(), v))
    }
}

#[derive(Debug, Clone)]
pub struct Stream {
    pub dict: Dictionary,
    /// Raw bytes as they appear in the file — filters have **not** been
    /// applied. Use [`crate::pdf::Document::decode_stream`] to materialise
    /// the decoded payload.
    pub content: Vec<u8>,
}
