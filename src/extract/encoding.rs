//! Built-in PDF byte-to-glyph-name encoding tables.
//!
//! PDF simple fonts declare an encoding by name (e.g. `/WinAnsiEncoding`)
//! plus an optional `/Differences` array that overrides individual byte
//! positions. We need the underlying name → glyph table to combine with
//! those differences.

/// The four built-in PDF encodings we support. Other encoding names (e.g.
/// `MacExpertEncoding`) fall through to `StandardEncoding`.
#[derive(Clone, Copy, Debug, Default)]
pub enum BaseEncoding {
    #[default]
    Standard,
    WinAnsi,
    MacRoman,
    Symbol,
}

impl BaseEncoding {
    pub fn from_name(name: &str) -> Self {
        match name {
            "WinAnsiEncoding" => Self::WinAnsi,
            "MacRomanEncoding" => Self::MacRoman,
            "SymbolEncoding" => Self::Symbol,
            _ => Self::Standard,
        }
    }

    pub fn glyph(self, byte: u8) -> Option<&'static str> {
        match self {
            Self::Standard => standard_encoding(byte),
            Self::WinAnsi => winansi_encoding(byte),
            Self::MacRoman => mac_roman_encoding(byte),
            Self::Symbol => symbol_encoding(byte),
        }
    }
}

/// Glyph names for the printable ASCII range, shared by every encoding.
fn ascii_glyph(byte: u8) -> Option<&'static str> {
    match byte {
        b' ' => Some("space"),
        b'!' => Some("exclam"),
        b'"' => Some("quotedbl"),
        b'#' => Some("numbersign"),
        b'$' => Some("dollar"),
        b'%' => Some("percent"),
        b'&' => Some("ampersand"),
        b'\'' => Some("quoteright"),
        b'(' => Some("parenleft"),
        b')' => Some("parenright"),
        b'*' => Some("asterisk"),
        b'+' => Some("plus"),
        b',' => Some("comma"),
        b'-' => Some("hyphen"),
        b'.' => Some("period"),
        b'/' => Some("slash"),
        b'0'..=b'9' => Some(DIGIT_NAMES[(byte - b'0') as usize]),
        b':' => Some("colon"),
        b';' => Some("semicolon"),
        b'<' => Some("less"),
        b'=' => Some("equal"),
        b'>' => Some("greater"),
        b'?' => Some("question"),
        b'@' => Some("at"),
        b'A'..=b'Z' => Some(UPPER_NAMES[(byte - b'A') as usize]),
        b'[' => Some("bracketleft"),
        b'\\' => Some("backslash"),
        b']' => Some("bracketright"),
        b'^' => Some("asciicircum"),
        b'_' => Some("underscore"),
        b'`' => Some("quoteleft"),
        b'a'..=b'z' => Some(LOWER_NAMES[(byte - b'a') as usize]),
        b'{' => Some("braceleft"),
        b'|' => Some("bar"),
        b'}' => Some("braceright"),
        b'~' => Some("asciitilde"),
        _ => None,
    }
}

const DIGIT_NAMES: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

const UPPER_NAMES: [&str; 26] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z",
];

const LOWER_NAMES: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];

fn standard_encoding(byte: u8) -> Option<&'static str> {
    if (0x20..0x7F).contains(&byte) {
        return ascii_glyph(byte);
    }
    match byte {
        0xA1 => Some("exclamdown"),
        0xA2 => Some("cent"),
        0xA3 => Some("sterling"),
        0xA4 => Some("fraction"),
        0xA5 => Some("yen"),
        0xA6 => Some("florin"),
        0xA7 => Some("section"),
        0xA8 => Some("currency"),
        0xA9 => Some("quotesingle"),
        0xAA => Some("quotedblleft"),
        0xAB => Some("guillemotleft"),
        0xAC => Some("guilsinglleft"),
        0xAD => Some("guilsinglright"),
        0xAE => Some("fi"),
        0xAF => Some("fl"),
        0xB1 => Some("endash"),
        0xB2 => Some("dagger"),
        0xB3 => Some("daggerdbl"),
        0xB4 => Some("periodcentered"),
        0xB6 => Some("paragraph"),
        0xB7 => Some("bullet"),
        0xB8 => Some("quotesinglbase"),
        0xB9 => Some("quotedblbase"),
        0xBA => Some("quotedblright"),
        0xBB => Some("guillemotright"),
        0xBC => Some("ellipsis"),
        0xBD => Some("perthousand"),
        0xBF => Some("questiondown"),
        0xC1 => Some("grave"),
        0xC2 => Some("acute"),
        0xC3 => Some("circumflex"),
        0xC4 => Some("tilde"),
        0xC5 => Some("macron"),
        0xC6 => Some("breve"),
        0xC7 => Some("dotaccent"),
        0xC8 => Some("dieresis"),
        0xCA => Some("ring"),
        0xCB => Some("cedilla"),
        0xCD => Some("hungarumlaut"),
        0xCE => Some("ogonek"),
        0xCF => Some("caron"),
        0xE1 => Some("AE"),
        0xE3 => Some("ordfeminine"),
        0xE8 => Some("Lslash"),
        0xE9 => Some("Oslash"),
        0xEA => Some("OE"),
        0xEB => Some("ordmasculine"),
        0xF1 => Some("ae"),
        0xF5 => Some("dotlessi"),
        0xF8 => Some("lslash"),
        0xF9 => Some("oslash"),
        0xFA => Some("oe"),
        0xFB => Some("germandbls"),
        _ => None,
    }
}

fn winansi_encoding(byte: u8) -> Option<&'static str> {
    if (0x20..0x7F).contains(&byte) {
        return ascii_glyph(byte);
    }
    match byte {
        0x80 => Some("Euro"),
        0x82 => Some("quotesinglbase"),
        0x83 => Some("florin"),
        0x84 => Some("quotedblbase"),
        0x85 => Some("ellipsis"),
        0x86 => Some("dagger"),
        0x87 => Some("daggerdbl"),
        0x88 => Some("circumflex"),
        0x89 => Some("perthousand"),
        0x8A => Some("Scaron"),
        0x8B => Some("guilsinglleft"),
        0x8C => Some("OE"),
        0x8E => Some("Zcaron"),
        0x91 => Some("quoteleft"),
        0x92 => Some("quoteright"),
        0x93 => Some("quotedblleft"),
        0x94 => Some("quotedblright"),
        0x95 => Some("bullet"),
        0x96 => Some("endash"),
        0x97 => Some("emdash"),
        0x98 => Some("tilde"),
        0x99 => Some("trademark"),
        0x9A => Some("scaron"),
        0x9B => Some("guilsinglright"),
        0x9C => Some("oe"),
        0x9E => Some("zcaron"),
        0x9F => Some("Ydieresis"),
        0xA0 => Some("space"),
        0xA1 => Some("exclamdown"),
        0xA2 => Some("cent"),
        0xA3 => Some("sterling"),
        0xA4 => Some("currency"),
        0xA5 => Some("yen"),
        0xA6 => Some("brokenbar"),
        0xA7 => Some("section"),
        0xA8 => Some("dieresis"),
        0xA9 => Some("copyright"),
        0xAA => Some("ordfeminine"),
        0xAB => Some("guillemotleft"),
        0xAC => Some("logicalnot"),
        0xAD => Some("hyphen"),
        0xAE => Some("registered"),
        0xAF => Some("macron"),
        0xB0 => Some("degree"),
        0xB1 => Some("plusminus"),
        0xB2 => Some("twosuperior"),
        0xB3 => Some("threesuperior"),
        0xB4 => Some("acute"),
        0xB5 => Some("mu"),
        0xB6 => Some("paragraph"),
        0xB7 => Some("periodcentered"),
        0xB8 => Some("cedilla"),
        0xB9 => Some("onesuperior"),
        0xBA => Some("ordmasculine"),
        0xBB => Some("guillemotright"),
        0xBC => Some("onequarter"),
        0xBD => Some("onehalf"),
        0xBE => Some("threequarters"),
        0xBF => Some("questiondown"),
        0xC0 => Some("Agrave"),
        0xC1 => Some("Aacute"),
        0xC2 => Some("Acircumflex"),
        0xC3 => Some("Atilde"),
        0xC4 => Some("Adieresis"),
        0xC5 => Some("Aring"),
        0xC6 => Some("AE"),
        0xC7 => Some("Ccedilla"),
        0xC8 => Some("Egrave"),
        0xC9 => Some("Eacute"),
        0xCA => Some("Ecircumflex"),
        0xCB => Some("Edieresis"),
        0xCC => Some("Igrave"),
        0xCD => Some("Iacute"),
        0xCE => Some("Icircumflex"),
        0xCF => Some("Idieresis"),
        0xD0 => Some("Eth"),
        0xD1 => Some("Ntilde"),
        0xD2 => Some("Ograve"),
        0xD3 => Some("Oacute"),
        0xD4 => Some("Ocircumflex"),
        0xD5 => Some("Otilde"),
        0xD6 => Some("Odieresis"),
        0xD7 => Some("multiply"),
        0xD8 => Some("Oslash"),
        0xD9 => Some("Ugrave"),
        0xDA => Some("Uacute"),
        0xDB => Some("Ucircumflex"),
        0xDC => Some("Udieresis"),
        0xDD => Some("Yacute"),
        0xDE => Some("Thorn"),
        0xDF => Some("germandbls"),
        0xE0 => Some("agrave"),
        0xE1 => Some("aacute"),
        0xE2 => Some("acircumflex"),
        0xE3 => Some("atilde"),
        0xE4 => Some("adieresis"),
        0xE5 => Some("aring"),
        0xE6 => Some("ae"),
        0xE7 => Some("ccedilla"),
        0xE8 => Some("egrave"),
        0xE9 => Some("eacute"),
        0xEA => Some("ecircumflex"),
        0xEB => Some("edieresis"),
        0xEC => Some("igrave"),
        0xED => Some("iacute"),
        0xEE => Some("icircumflex"),
        0xEF => Some("idieresis"),
        0xF0 => Some("eth"),
        0xF1 => Some("ntilde"),
        0xF2 => Some("ograve"),
        0xF3 => Some("oacute"),
        0xF4 => Some("ocircumflex"),
        0xF5 => Some("otilde"),
        0xF6 => Some("odieresis"),
        0xF7 => Some("divide"),
        0xF8 => Some("oslash"),
        0xF9 => Some("ugrave"),
        0xFA => Some("uacute"),
        0xFB => Some("ucircumflex"),
        0xFC => Some("udieresis"),
        0xFD => Some("yacute"),
        0xFE => Some("thorn"),
        0xFF => Some("ydieresis"),
        _ => None,
    }
}

fn mac_roman_encoding(byte: u8) -> Option<&'static str> {
    if (0x20..0x7F).contains(&byte) {
        return ascii_glyph(byte);
    }
    match byte {
        0x80 => Some("Adieresis"),
        0x81 => Some("Aring"),
        0x82 => Some("Ccedilla"),
        0x83 => Some("Eacute"),
        0x84 => Some("Ntilde"),
        0x85 => Some("Odieresis"),
        0x86 => Some("Udieresis"),
        0x87 => Some("aacute"),
        0x88 => Some("agrave"),
        0x89 => Some("acircumflex"),
        0x8A => Some("adieresis"),
        0x8B => Some("atilde"),
        0x8C => Some("aring"),
        0x8D => Some("ccedilla"),
        0x8E => Some("eacute"),
        0x8F => Some("egrave"),
        0x90 => Some("ecircumflex"),
        0x91 => Some("edieresis"),
        0x92 => Some("iacute"),
        0x93 => Some("igrave"),
        0x94 => Some("icircumflex"),
        0x95 => Some("idieresis"),
        0x96 => Some("ntilde"),
        0x97 => Some("oacute"),
        0x98 => Some("ograve"),
        0x99 => Some("ocircumflex"),
        0x9A => Some("odieresis"),
        0x9B => Some("otilde"),
        0x9C => Some("uacute"),
        0x9D => Some("ugrave"),
        0x9E => Some("ucircumflex"),
        0x9F => Some("udieresis"),
        0xA0 => Some("dagger"),
        0xA1 => Some("degree"),
        0xA2 => Some("cent"),
        0xA3 => Some("sterling"),
        0xA4 => Some("section"),
        0xA5 => Some("bullet"),
        0xA6 => Some("paragraph"),
        0xA7 => Some("germandbls"),
        0xA8 => Some("registered"),
        0xA9 => Some("copyright"),
        0xAA => Some("trademark"),
        0xAB => Some("acute"),
        0xAC => Some("dieresis"),
        0xAD => Some("notequal"),
        0xAE => Some("AE"),
        0xAF => Some("Oslash"),
        0xB0 => Some("infinity"),
        0xB1 => Some("plusminus"),
        0xB2 => Some("lessequal"),
        0xB3 => Some("greaterequal"),
        0xB4 => Some("yen"),
        0xB5 => Some("mu"),
        0xB6 => Some("partialdiff"),
        0xB7 => Some("summation"),
        0xB8 => Some("product"),
        0xB9 => Some("pi"),
        0xBA => Some("integral"),
        0xBB => Some("ordfeminine"),
        0xBC => Some("ordmasculine"),
        0xBD => Some("Omega"),
        0xBE => Some("ae"),
        0xBF => Some("oslash"),
        0xC0 => Some("questiondown"),
        0xC1 => Some("exclamdown"),
        0xC2 => Some("logicalnot"),
        0xC3 => Some("radical"),
        0xC4 => Some("florin"),
        0xC5 => Some("approxequal"),
        0xC6 => Some("Delta"),
        0xC7 => Some("guillemotleft"),
        0xC8 => Some("guillemotright"),
        0xC9 => Some("ellipsis"),
        0xCA => Some("space"),
        0xCB => Some("Agrave"),
        0xCC => Some("Atilde"),
        0xCD => Some("Otilde"),
        0xCE => Some("OE"),
        0xCF => Some("oe"),
        0xD0 => Some("endash"),
        0xD1 => Some("emdash"),
        0xD2 => Some("quotedblleft"),
        0xD3 => Some("quotedblright"),
        0xD4 => Some("quoteleft"),
        0xD5 => Some("quoteright"),
        0xD6 => Some("divide"),
        0xD7 => Some("lozenge"),
        0xD8 => Some("ydieresis"),
        0xD9 => Some("Ydieresis"),
        0xDA => Some("fraction"),
        0xDB => Some("currency"),
        0xDC => Some("guilsinglleft"),
        0xDD => Some("guilsinglright"),
        0xDE => Some("fi"),
        0xDF => Some("fl"),
        0xE0 => Some("daggerdbl"),
        0xE1 => Some("periodcentered"),
        0xE2 => Some("quotesinglbase"),
        0xE3 => Some("quotedblbase"),
        0xE4 => Some("perthousand"),
        0xE5 => Some("Acircumflex"),
        0xE6 => Some("Ecircumflex"),
        0xE7 => Some("Aacute"),
        0xE8 => Some("Edieresis"),
        0xE9 => Some("Egrave"),
        0xEA => Some("Iacute"),
        0xEB => Some("Icircumflex"),
        0xEC => Some("Idieresis"),
        0xED => Some("Igrave"),
        0xEE => Some("Oacute"),
        0xEF => Some("Ocircumflex"),
        0xF1 => Some("Ograve"),
        0xF2 => Some("Uacute"),
        0xF3 => Some("Ucircumflex"),
        0xF4 => Some("Ugrave"),
        0xF5 => Some("dotlessi"),
        0xF6 => Some("circumflex"),
        0xF7 => Some("tilde"),
        0xF8 => Some("macron"),
        0xF9 => Some("breve"),
        0xFA => Some("dotaccent"),
        0xFB => Some("ring"),
        0xFC => Some("cedilla"),
        0xFD => Some("hungarumlaut"),
        0xFE => Some("ogonek"),
        0xFF => Some("caron"),
        _ => None,
    }
}

fn symbol_encoding(byte: u8) -> Option<&'static str> {
    // Symbol font has a unique mapping; cover the Greek letters and common
    // math operators that actually show up in extracted text.
    match byte {
        0x20 => Some("space"),
        0x21 => Some("exclam"),
        0x22 => Some("universal"),
        0x23 => Some("numbersign"),
        0x24 => Some("existential"),
        0x25 => Some("percent"),
        0x26 => Some("ampersand"),
        0x27 => Some("suchthat"),
        0x28 => Some("parenleft"),
        0x29 => Some("parenright"),
        0x2A => Some("asteriskmath"),
        0x2B => Some("plus"),
        0x2C => Some("comma"),
        0x2D => Some("minus"),
        0x2E => Some("period"),
        0x2F => Some("slash"),
        0x30..=0x39 => DIGIT_NAMES.get((byte - b'0') as usize).copied(),
        0x3A => Some("colon"),
        0x3B => Some("semicolon"),
        0x3C => Some("less"),
        0x3D => Some("equal"),
        0x3E => Some("greater"),
        0x3F => Some("question"),
        0x40 => Some("congruent"),
        0x41 => Some("Alpha"),
        0x42 => Some("Beta"),
        0x43 => Some("Chi"),
        0x44 => Some("Delta"),
        0x45 => Some("Epsilon"),
        0x46 => Some("Phi"),
        0x47 => Some("Gamma"),
        0x48 => Some("Eta"),
        0x49 => Some("Iota"),
        0x4A => Some("theta1"),
        0x4B => Some("Kappa"),
        0x4C => Some("Lambda"),
        0x4D => Some("Mu"),
        0x4E => Some("Nu"),
        0x4F => Some("Omicron"),
        0x50 => Some("Pi"),
        0x51 => Some("Theta"),
        0x52 => Some("Rho"),
        0x53 => Some("Sigma"),
        0x54 => Some("Tau"),
        0x55 => Some("Upsilon"),
        0x56 => Some("sigma1"),
        0x57 => Some("Omega"),
        0x58 => Some("Xi"),
        0x59 => Some("Psi"),
        0x5A => Some("Zeta"),
        0x61 => Some("alpha"),
        0x62 => Some("beta"),
        0x63 => Some("chi"),
        0x64 => Some("delta"),
        0x65 => Some("epsilon"),
        0x66 => Some("phi"),
        0x67 => Some("gamma"),
        0x68 => Some("eta"),
        0x69 => Some("iota"),
        0x6A => Some("phi1"),
        0x6B => Some("kappa"),
        0x6C => Some("lambda"),
        0x6D => Some("mu"),
        0x6E => Some("nu"),
        0x6F => Some("omicron"),
        0x70 => Some("pi"),
        0x71 => Some("theta"),
        0x72 => Some("rho"),
        0x73 => Some("sigma"),
        0x74 => Some("tau"),
        0x75 => Some("upsilon"),
        0x76 => Some("omega1"),
        0x77 => Some("omega"),
        0x78 => Some("xi"),
        0x79 => Some("psi"),
        0x7A => Some("zeta"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_for(enc: BaseEncoding) -> Vec<(u8, &'static str)> {
        (0u8..=255)
            .filter_map(|b| enc.glyph(b).map(|name| (b, name)))
            .collect()
    }

    #[test]
    fn each_encoding_dispatches_to_its_table() {
        // Touch the dispatch and bottom-out match arms in every helper.
        assert_eq!(BaseEncoding::Standard.glyph(b'A'), Some("A"));
        assert_eq!(BaseEncoding::WinAnsi.glyph(b'A'), Some("A"));
        assert_eq!(BaseEncoding::MacRoman.glyph(b'A'), Some("A"));
        assert_eq!(BaseEncoding::Symbol.glyph(0x41), Some("Alpha"));
        // Bytes that map to nothing under each table.
        assert!(BaseEncoding::Standard.glyph(0x80).is_none());
        assert!(BaseEncoding::WinAnsi.glyph(0x7F).is_none());
        assert!(BaseEncoding::MacRoman.glyph(0xF0).is_none());
        assert!(BaseEncoding::Symbol.glyph(0xFF).is_none());
    }

    #[test]
    fn from_name_dispatch_covers_each_variant() {
        assert!(matches!(
            BaseEncoding::from_name("WinAnsiEncoding"),
            BaseEncoding::WinAnsi
        ));
        assert!(matches!(
            BaseEncoding::from_name("MacRomanEncoding"),
            BaseEncoding::MacRoman
        ));
        assert!(matches!(
            BaseEncoding::from_name("SymbolEncoding"),
            BaseEncoding::Symbol
        ));
        assert!(matches!(
            BaseEncoding::from_name("MacExpertEncoding"),
            BaseEncoding::Standard
        ));
    }

    #[test]
    fn ascii_glyph_covers_each_punctuation_arm() {
        // Walk every printable ASCII byte through the shared ASCII table.
        for b in 0x20u8..0x7F {
            assert!(
                BaseEncoding::WinAnsi.glyph(b).is_some(),
                "WinAnsi missing 0x{b:02X}"
            );
        }
    }

    #[test]
    fn every_encoding_returns_some_for_a_meaningful_byte() {
        for enc in [
            BaseEncoding::Standard,
            BaseEncoding::WinAnsi,
            BaseEncoding::MacRoman,
            BaseEncoding::Symbol,
        ] {
            // Exhaustive sweep — runs every match arm at least once.
            for b in 0u8..=255 {
                let _ = enc.glyph(b);
            }
        }
        // Cross-check a handful of known mappings per table so the sweep
        // doesn't pass just by going through the motions.
        assert_eq!(BaseEncoding::Standard.glyph(0xAE), Some("fi"));
        assert_eq!(BaseEncoding::WinAnsi.glyph(0x80), Some("Euro"));
        assert_eq!(BaseEncoding::MacRoman.glyph(0xA0), Some("dagger"));
        assert_eq!(BaseEncoding::Symbol.glyph(0x71), Some("theta"));
    }

    #[test]
    fn winansi_and_macroman_have_full_high_range_tables() {
        // Each of the high-range tables must contribute meaningful entries,
        // not just delegate to ASCII; otherwise the sweep above wouldn't
        // exercise the long match arms in the source.
        let winansi = names_for(BaseEncoding::WinAnsi);
        let macroman = names_for(BaseEncoding::MacRoman);
        assert!(winansi.len() > 200, "winansi only has {} entries", winansi.len());
        assert!(macroman.len() > 200, "macroman only has {} entries", macroman.len());
    }
}
