//! Glyph-name → Unicode lookup.
//!
//! Subset of the Adobe Glyph List sufficient for academic / English text:
//! ASCII, Latin-1 letters, common punctuation, math operators, Greek, and
//! the f-ligatures. Names not in the table fall through to the `uniXXXX` /
//! `uXXXXXXXX` and `cidNNNN` synthetic conventions.

/// Map a PDF glyph name to its Unicode representation.
pub fn glyph_to_string(name: &str) -> Option<String> {
    if let Some(s) = lookup(name) {
        return Some(s.to_string());
    }

    if let Some(rest) = name.strip_prefix("uni") {
        return decode_uni_sequence(rest);
    }
    if let Some(rest) = name.strip_prefix('u') {
        if let Some(c) = parse_hex_char(rest) {
            return Some(c.to_string());
        }
    }

    // ".notdef" and unknown names: emit nothing rather than guess.
    None
}

fn decode_uni_sequence(rest: &str) -> Option<String> {
    // `uni` glyph names concatenate one or more 4-digit hex codes.
    if rest.is_empty() || !rest.len().is_multiple_of(4) {
        return None;
    }
    let mut out = String::new();
    for chunk in rest.as_bytes().chunks(4) {
        let hex = std::str::from_utf8(chunk).ok()?;
        let cp = u32::from_str_radix(hex, 16).ok()?;
        let ch = char::from_u32(cp)?;
        out.push(ch);
    }
    Some(out)
}

fn parse_hex_char(s: &str) -> Option<char> {
    // 4 to 6 hex digits per the convention.
    if !(4..=6).contains(&s.len()) {
        return None;
    }
    let cp = u32::from_str_radix(s, 16).ok()?;
    char::from_u32(cp)
}

fn lookup(name: &str) -> Option<&'static str> {
    Some(match name {
        // Single ASCII letters & digits - many PDFs use these as glyph names.
        "A" => "A",
        "B" => "B",
        "C" => "C",
        "D" => "D",
        "E" => "E",
        "F" => "F",
        "G" => "G",
        "H" => "H",
        "I" => "I",
        "J" => "J",
        "K" => "K",
        "L" => "L",
        "M" => "M",
        "N" => "N",
        "O" => "O",
        "P" => "P",
        "Q" => "Q",
        "R" => "R",
        "S" => "S",
        "T" => "T",
        "U" => "U",
        "V" => "V",
        "W" => "W",
        "X" => "X",
        "Y" => "Y",
        "Z" => "Z",
        "a" => "a",
        "b" => "b",
        "c" => "c",
        "d" => "d",
        "e" => "e",
        "f" => "f",
        "g" => "g",
        "h" => "h",
        "i" => "i",
        "j" => "j",
        "k" => "k",
        "l" => "l",
        "m" => "m",
        "n" => "n",
        "o" => "o",
        "p" => "p",
        "q" => "q",
        "r" => "r",
        "s" => "s",
        "t" => "t",
        "u" => "u",
        "v" => "v",
        "w" => "w",
        "x" => "x",
        "y" => "y",
        "z" => "z",
        "zero" => "0",
        "one" => "1",
        "two" => "2",
        "three" => "3",
        "four" => "4",
        "five" => "5",
        "six" => "6",
        "seven" => "7",
        "eight" => "8",
        "nine" => "9",

        // ASCII punctuation
        "space" => " ",
        "nbspace" => "\u{00A0}",
        "nbsp" => "\u{00A0}",
        "exclam" => "!",
        "quotedbl" => "\"",
        "numbersign" => "#",
        "dollar" => "$",
        "percent" => "%",
        "ampersand" => "&",
        "quotesingle" => "'",
        "quoteright" => "\u{2019}",
        "quoteleft" => "\u{2018}",
        "parenleft" => "(",
        "parenright" => ")",
        "asterisk" => "*",
        "plus" => "+",
        "comma" => ",",
        "hyphen" => "-",
        "minus" => "\u{2212}",
        "period" => ".",
        "slash" => "/",
        "colon" => ":",
        "semicolon" => ";",
        "less" => "<",
        "equal" => "=",
        "greater" => ">",
        "question" => "?",
        "at" => "@",
        "bracketleft" => "[",
        "backslash" => "\\",
        "bracketright" => "]",
        "asciicircum" => "^",
        "underscore" => "_",
        "grave" => "`",
        "braceleft" => "{",
        "bar" => "|",
        "braceright" => "}",
        "asciitilde" => "~",

        // Latin-1 punctuation
        "exclamdown" => "\u{00A1}",
        "cent" => "\u{00A2}",
        "sterling" => "\u{00A3}",
        "currency" => "\u{00A4}",
        "yen" => "\u{00A5}",
        "brokenbar" => "\u{00A6}",
        "section" => "\u{00A7}",
        "dieresis" => "\u{00A8}",
        "copyright" => "\u{00A9}",
        "ordfeminine" => "\u{00AA}",
        "guillemotleft" => "\u{00AB}",
        "logicalnot" => "\u{00AC}",
        "registered" => "\u{00AE}",
        "macron" => "\u{00AF}",
        "degree" => "\u{00B0}",
        "plusminus" => "\u{00B1}",
        "twosuperior" => "\u{00B2}",
        "threesuperior" => "\u{00B3}",
        "acute" => "\u{00B4}",
        "mu" => "\u{03BC}",
        "paragraph" => "\u{00B6}",
        "periodcentered" => "\u{00B7}",
        "cedilla" => "\u{00B8}",
        "onesuperior" => "\u{00B9}",
        "ordmasculine" => "\u{00BA}",
        "guillemotright" => "\u{00BB}",
        "onequarter" => "\u{00BC}",
        "onehalf" => "\u{00BD}",
        "threequarters" => "\u{00BE}",
        "questiondown" => "\u{00BF}",

        // Latin accented uppercase
        "Agrave" => "\u{00C0}",
        "Aacute" => "\u{00C1}",
        "Acircumflex" => "\u{00C2}",
        "Atilde" => "\u{00C3}",
        "Adieresis" => "\u{00C4}",
        "Aring" => "\u{00C5}",
        "AE" => "\u{00C6}",
        "Ccedilla" => "\u{00C7}",
        "Egrave" => "\u{00C8}",
        "Eacute" => "\u{00C9}",
        "Ecircumflex" => "\u{00CA}",
        "Edieresis" => "\u{00CB}",
        "Igrave" => "\u{00CC}",
        "Iacute" => "\u{00CD}",
        "Icircumflex" => "\u{00CE}",
        "Idieresis" => "\u{00CF}",
        "Eth" => "\u{00D0}",
        "Ntilde" => "\u{00D1}",
        "Ograve" => "\u{00D2}",
        "Oacute" => "\u{00D3}",
        "Ocircumflex" => "\u{00D4}",
        "Otilde" => "\u{00D5}",
        "Odieresis" => "\u{00D6}",
        "multiply" => "\u{00D7}",
        "Oslash" => "\u{00D8}",
        "Ugrave" => "\u{00D9}",
        "Uacute" => "\u{00DA}",
        "Ucircumflex" => "\u{00DB}",
        "Udieresis" => "\u{00DC}",
        "Yacute" => "\u{00DD}",
        "Thorn" => "\u{00DE}",
        "germandbls" => "\u{00DF}",

        // Latin accented lowercase
        "agrave" => "\u{00E0}",
        "aacute" => "\u{00E1}",
        "acircumflex" => "\u{00E2}",
        "atilde" => "\u{00E3}",
        "adieresis" => "\u{00E4}",
        "aring" => "\u{00E5}",
        "ae" => "\u{00E6}",
        "ccedilla" => "\u{00E7}",
        "egrave" => "\u{00E8}",
        "eacute" => "\u{00E9}",
        "ecircumflex" => "\u{00EA}",
        "edieresis" => "\u{00EB}",
        "igrave" => "\u{00EC}",
        "iacute" => "\u{00ED}",
        "icircumflex" => "\u{00EE}",
        "idieresis" => "\u{00EF}",
        "eth" => "\u{00F0}",
        "ntilde" => "\u{00F1}",
        "ograve" => "\u{00F2}",
        "oacute" => "\u{00F3}",
        "ocircumflex" => "\u{00F4}",
        "otilde" => "\u{00F5}",
        "odieresis" => "\u{00F6}",
        "divide" => "\u{00F7}",
        "oslash" => "\u{00F8}",
        "ugrave" => "\u{00F9}",
        "uacute" => "\u{00FA}",
        "ucircumflex" => "\u{00FB}",
        "udieresis" => "\u{00FC}",
        "yacute" => "\u{00FD}",
        "thorn" => "\u{00FE}",
        "ydieresis" => "\u{00FF}",
        "Ydieresis" => "\u{0178}",

        // Common typographic punctuation
        "quotedblleft" => "\u{201C}",
        "quotedblright" => "\u{201D}",
        "quotesinglbase" => "\u{201A}",
        "quotedblbase" => "\u{201E}",
        "endash" => "\u{2013}",
        "emdash" => "\u{2014}",
        "ellipsis" => "\u{2026}",
        "bullet" => "\u{2022}",
        "dagger" => "\u{2020}",
        "daggerdbl" => "\u{2021}",
        "guilsinglleft" => "\u{2039}",
        "guilsinglright" => "\u{203A}",
        "perthousand" => "\u{2030}",
        "trademark" => "\u{2122}",
        "fraction" => "\u{2044}",
        "Euro" => "\u{20AC}",
        "florin" => "\u{0192}",
        "Scaron" => "\u{0160}",
        "scaron" => "\u{0161}",
        "Zcaron" => "\u{017D}",
        "zcaron" => "\u{017E}",
        "OE" => "\u{0152}",
        "oe" => "\u{0153}",
        "Lslash" => "\u{0141}",
        "lslash" => "\u{0142}",
        "circumflex" => "\u{02C6}",
        "caron" => "\u{02C7}",
        "tilde" => "\u{02DC}",
        "breve" => "\u{02D8}",
        "dotaccent" => "\u{02D9}",
        "ring" => "\u{02DA}",
        "hungarumlaut" => "\u{02DD}",
        "ogonek" => "\u{02DB}",
        "dotlessi" => "\u{0131}",

        // f-ligatures (frequently emitted by TeX)
        "fi" => "fi",
        "fl" => "fl",
        "ff" => "ff",
        "ffi" => "ffi",
        "ffl" => "ffl",

        // Greek upper
        "Alpha" => "\u{0391}",
        "Beta" => "\u{0392}",
        "Gamma" => "\u{0393}",
        "Delta" => "\u{0394}",
        "Epsilon" => "\u{0395}",
        "Zeta" => "\u{0396}",
        "Eta" => "\u{0397}",
        "Theta" => "\u{0398}",
        "Iota" => "\u{0399}",
        "Kappa" => "\u{039A}",
        "Lambda" => "\u{039B}",
        "Mu" => "\u{039C}",
        "Nu" => "\u{039D}",
        "Xi" => "\u{039E}",
        "Omicron" => "\u{039F}",
        "Pi" => "\u{03A0}",
        "Rho" => "\u{03A1}",
        "Sigma" => "\u{03A3}",
        "Tau" => "\u{03A4}",
        "Upsilon" => "\u{03A5}",
        "Phi" => "\u{03A6}",
        "Chi" => "\u{03A7}",
        "Psi" => "\u{03A8}",
        "Omega" => "\u{03A9}",

        // Greek lower
        "alpha" => "\u{03B1}",
        "beta" => "\u{03B2}",
        "gamma" => "\u{03B3}",
        "delta" => "\u{03B4}",
        "epsilon" => "\u{03B5}",
        "zeta" => "\u{03B6}",
        "eta" => "\u{03B7}",
        "theta" => "\u{03B8}",
        "iota" => "\u{03B9}",
        "kappa" => "\u{03BA}",
        "lambda" => "\u{03BB}",
        "nu" => "\u{03BD}",
        "xi" => "\u{03BE}",
        "omicron" => "\u{03BF}",
        "pi" => "\u{03C0}",
        "rho" => "\u{03C1}",
        "sigma" => "\u{03C3}",
        "tau" => "\u{03C4}",
        "upsilon" => "\u{03C5}",
        "phi" => "\u{03C6}",
        "chi" => "\u{03C7}",
        "psi" => "\u{03C8}",
        "omega" => "\u{03C9}",
        "theta1" => "\u{03D1}",
        "phi1" => "\u{03D5}",
        "omega1" => "\u{03D6}",
        "sigma1" => "\u{03C2}",

        // Math operators that show up in our sample paper
        "summation" => "\u{2211}",
        "product" => "\u{220F}",
        "integral" => "\u{222B}",
        "partialdiff" => "\u{2202}",
        "radical" => "\u{221A}",
        "infinity" => "\u{221E}",
        "approxequal" => "\u{2248}",
        "notequal" => "\u{2260}",
        "lessequal" => "\u{2264}",
        "greaterequal" => "\u{2265}",
        "congruent" => "\u{2245}",
        "existential" => "\u{2203}",
        "universal" => "\u{2200}",
        "suchthat" => "\u{220B}",
        "asteriskmath" => "\u{2217}",
        "lozenge" => "\u{25CA}",

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letters_round_trip() {
        assert_eq!(glyph_to_string("A").as_deref(), Some("A"));
        assert_eq!(glyph_to_string("z").as_deref(), Some("z"));
    }

    #[test]
    fn ligatures_resolve() {
        assert_eq!(glyph_to_string("fi").as_deref(), Some("fi"));
        assert_eq!(glyph_to_string("ffl").as_deref(), Some("ffl"));
    }

    #[test]
    fn uni_hex_fallback() {
        // Greek small letter alpha encoded as `uni03B1`.
        assert_eq!(glyph_to_string("uni03B1").as_deref(), Some("α"));
    }

    #[test]
    fn unknown_glyph_returns_none() {
        assert!(glyph_to_string("definitely_not_a_glyph").is_none());
    }
}
