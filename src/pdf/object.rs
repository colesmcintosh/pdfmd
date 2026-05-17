//! PDF object model: just the variants the text extractor cares about.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub u32, pub u16);

#[derive(Debug, Clone)]
pub enum Object {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f32),
    String(Vec<u8>),
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
    // These accessors round out the variants — they're only consumed by
    // the test suite today, but keeping them on the public surface lets
    // future callers extract any Object kind without reaching inside the
    // enum.
    #[allow(dead_code)]
    pub fn as_string(&self) -> Option<&[u8]> {
        if let Object::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    #[allow(dead_code)]
    pub fn as_real(&self) -> Option<f32> {
        match self {
            Object::Real(r) => Some(*r),
            Object::Integer(i) => Some(*i as f32),
            _ => None,
        }
    }
    #[allow(dead_code)]
    pub fn as_boolean(&self) -> Option<bool> {
        if let Object::Boolean(b) = self {
            Some(*b)
        } else {
            None
        }
    }
    #[allow(dead_code)]
    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_objects() -> Vec<Object> {
        vec![
            Object::Null,
            Object::Boolean(true),
            Object::Integer(7),
            Object::Real(2.5),
            Object::String(b"hi".to_vec()),
            Object::Name(b"Foo".to_vec()),
            Object::Array(vec![Object::Integer(1), Object::Integer(2)]),
            Object::Dictionary({
                let mut d = Dictionary::new();
                d.insert(b"k".to_vec(), Object::Integer(1));
                d
            }),
            Object::Stream(Stream {
                dict: Dictionary::new(),
                content: vec![1, 2, 3],
            }),
            Object::Reference(ObjectId(9, 0)),
        ]
    }

    #[test]
    fn accessors_return_some_only_for_matching_variants() {
        for obj in sample_objects() {
            let expect_dict = matches!(obj, Object::Dictionary(_));
            let expect_array = matches!(obj, Object::Array(_));
            let expect_name = matches!(obj, Object::Name(_));
            let expect_int_or_real = matches!(obj, Object::Integer(_) | Object::Real(_));
            let expect_stream = matches!(obj, Object::Stream(_));
            let expect_ref = matches!(obj, Object::Reference(_));
            let expect_string = matches!(obj, Object::String(_));
            let expect_real_like = matches!(obj, Object::Integer(_) | Object::Real(_));
            let expect_bool = matches!(obj, Object::Boolean(_));
            assert_eq!(obj.as_dict().is_some(), expect_dict);
            assert_eq!(obj.as_array().is_some(), expect_array);
            assert_eq!(obj.as_name().is_some(), expect_name);
            assert_eq!(obj.as_name_str().is_some(), expect_name);
            assert_eq!(obj.as_integer().is_some(), expect_int_or_real);
            assert_eq!(obj.as_stream().is_some(), expect_stream);
            assert_eq!(obj.as_reference().is_some(), expect_ref);
            assert_eq!(obj.as_string().is_some(), expect_string);
            assert_eq!(obj.as_real().is_some(), expect_real_like);
            assert_eq!(obj.as_boolean().is_some(), expect_bool);
            assert_eq!(obj.is_null(), matches!(obj, Object::Null));
        }
    }

    #[test]
    fn as_integer_truncates_reals() {
        assert_eq!(Object::Real(3.9).as_integer(), Some(3));
        assert_eq!(Object::Real(-1.5).as_integer(), Some(-1));
    }

    #[test]
    fn as_name_str_returns_none_for_invalid_utf8() {
        // Lone 0xFF byte — not valid UTF-8.
        let obj = Object::Name(vec![0xFFu8]);
        assert!(obj.as_name_str().is_none());
        // Valid UTF-8 round-trips.
        assert_eq!(Object::Name(b"hi".to_vec()).as_name_str(), Some("hi"));
    }

    #[test]
    fn dictionary_insert_replaces_existing_value() {
        let mut d = Dictionary::new();
        d.insert(b"k".to_vec(), Object::Integer(1));
        d.insert(b"k".to_vec(), Object::Integer(2));
        assert_eq!(d.get(b"k").and_then(Object::as_integer), Some(2));
        // iter yields the single entry.
        let v: Vec<_> = d.iter().collect();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn dictionary_get_returns_none_for_missing_key() {
        let d = Dictionary::new();
        assert!(d.get(b"missing").is_none());
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
