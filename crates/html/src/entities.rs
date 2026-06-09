//! HTML character-reference tables and lookup helpers.
//!
//! `NAMED_ENTITIES` is keyed *with* the trailing `;` and sorted by name;
//! `LEGACY_ENTITIES` holds the semicolonless legacy set keyed *without* `;`.
//! Values are `&'static str` so multi-codepoint replacements work. A practical
//! HTML5 subset (not the full ~2200-entry WHATWG list).

/// Named references, keyed WITH the trailing `;`, sorted by name (binary search).
/// Replacement is a UTF-8 string (multi-codepoint entities supported).
pub(crate) static NAMED_ENTITIES: &[(&str, &str)] = &[
    ("AElig;", "\u{00C6}"),
    ("Aacute;", "\u{00C1}"),
    ("Acirc;", "\u{00C2}"),
    ("Agrave;", "\u{00C0}"),
    ("Alpha;", "\u{0391}"),
    ("Aring;", "\u{00C5}"),
    ("Atilde;", "\u{00C3}"),
    ("Auml;", "\u{00C4}"),
    ("Beta;", "\u{0392}"),
    ("Ccedil;", "\u{00C7}"),
    ("Chi;", "\u{03A7}"),
    ("Dagger;", "\u{2021}"),
    ("Delta;", "\u{0394}"),
    ("ETH;", "\u{00D0}"),
    ("Eacute;", "\u{00C9}"),
    ("Ecirc;", "\u{00CA}"),
    ("Egrave;", "\u{00C8}"),
    ("Epsilon;", "\u{0395}"),
    ("Eta;", "\u{0397}"),
    ("Euml;", "\u{00CB}"),
    ("Gamma;", "\u{0393}"),
    ("Iacute;", "\u{00CD}"),
    ("Icirc;", "\u{00CE}"),
    ("Igrave;", "\u{00CC}"),
    ("Iota;", "\u{0399}"),
    ("Iuml;", "\u{00CF}"),
    ("Kappa;", "\u{039A}"),
    ("Lambda;", "\u{039B}"),
    ("Mu;", "\u{039C}"),
    ("Ntilde;", "\u{00D1}"),
    ("Nu;", "\u{039D}"),
    ("OElig;", "\u{0152}"),
    ("Oacute;", "\u{00D3}"),
    ("Ocirc;", "\u{00D4}"),
    ("Ograve;", "\u{00D2}"),
    ("Omega;", "\u{03A9}"),
    ("Omicron;", "\u{039F}"),
    ("Oslash;", "\u{00D8}"),
    ("Otilde;", "\u{00D5}"),
    ("Ouml;", "\u{00D6}"),
    ("Phi;", "\u{03A6}"),
    ("Pi;", "\u{03A0}"),
    ("Prime;", "\u{2033}"),
    ("Psi;", "\u{03A8}"),
    ("Rho;", "\u{03A1}"),
    ("Scaron;", "\u{0160}"),
    ("Sigma;", "\u{03A3}"),
    ("THORN;", "\u{00DE}"),
    ("Tau;", "\u{03A4}"),
    ("Theta;", "\u{0398}"),
    ("Uacute;", "\u{00DA}"),
    ("Ucirc;", "\u{00DB}"),
    ("Ugrave;", "\u{00D9}"),
    ("Upsilon;", "\u{03A5}"),
    ("Uuml;", "\u{00DC}"),
    ("Xi;", "\u{039E}"),
    ("Yacute;", "\u{00DD}"),
    ("Yuml;", "\u{0178}"),
    ("Zeta;", "\u{0396}"),
    ("aacute;", "\u{00E1}"),
    ("acirc;", "\u{00E2}"),
    ("acute;", "\u{00B4}"),
    ("aelig;", "\u{00E6}"),
    ("agrave;", "\u{00E0}"),
    ("alpha;", "\u{03B1}"),
    ("amp;", "&"),
    ("and;", "\u{2227}"),
    ("ang;", "\u{2220}"),
    ("apos;", "'"),
    ("aring;", "\u{00E5}"),
    ("asymp;", "\u{2248}"),
    ("atilde;", "\u{00E3}"),
    ("auml;", "\u{00E4}"),
    ("bdquo;", "\u{201E}"),
    ("beta;", "\u{03B2}"),
    ("brvbar;", "\u{00A6}"),
    ("bull;", "\u{2022}"),
    ("cap;", "\u{2229}"),
    ("ccedil;", "\u{00E7}"),
    ("cedil;", "\u{00B8}"),
    ("cent;", "\u{00A2}"),
    ("chi;", "\u{03C7}"),
    ("clubs;", "\u{2663}"),
    ("cong;", "\u{2245}"),
    ("copy;", "\u{00A9}"),
    ("crarr;", "\u{21B5}"),
    ("cup;", "\u{222A}"),
    ("curren;", "\u{00A4}"),
    ("dArr;", "\u{21D3}"),
    ("dagger;", "\u{2020}"),
    ("darr;", "\u{2193}"),
    ("deg;", "\u{00B0}"),
    ("delta;", "\u{03B4}"),
    ("diams;", "\u{2666}"),
    ("divide;", "\u{00F7}"),
    ("eacute;", "\u{00E9}"),
    ("ecirc;", "\u{00EA}"),
    ("egrave;", "\u{00E8}"),
    ("empty;", "\u{2205}"),
    ("emsp;", "\u{2003}"),
    ("ensp;", "\u{2002}"),
    ("epsilon;", "\u{03B5}"),
    ("equiv;", "\u{2261}"),
    ("eta;", "\u{03B7}"),
    ("eth;", "\u{00F0}"),
    ("euml;", "\u{00EB}"),
    ("euro;", "\u{20AC}"),
    ("exist;", "\u{2203}"),
    ("fnof;", "\u{0192}"),
    ("forall;", "\u{2200}"),
    ("frac12;", "\u{00BD}"),
    ("frac14;", "\u{00BC}"),
    ("frac34;", "\u{00BE}"),
    ("gamma;", "\u{03B3}"),
    ("ge;", "\u{2265}"),
    ("gt;", ">"),
    ("hArr;", "\u{21D4}"),
    ("harr;", "\u{2194}"),
    ("hearts;", "\u{2665}"),
    ("hellip;", "\u{2026}"),
    ("iacute;", "\u{00ED}"),
    ("icirc;", "\u{00EE}"),
    ("iexcl;", "\u{00A1}"),
    ("igrave;", "\u{00EC}"),
    ("infin;", "\u{221E}"),
    ("int;", "\u{222B}"),
    ("iota;", "\u{03B9}"),
    ("iquest;", "\u{00BF}"),
    ("isin;", "\u{2208}"),
    ("iuml;", "\u{00EF}"),
    ("kappa;", "\u{03BA}"),
    ("lArr;", "\u{21D0}"),
    ("lambda;", "\u{03BB}"),
    ("laquo;", "\u{00AB}"),
    ("larr;", "\u{2190}"),
    ("ldquo;", "\u{201C}"),
    ("le;", "\u{2264}"),
    ("lowast;", "\u{2217}"),
    ("lrm;", "\u{200E}"),
    ("lsaquo;", "\u{2039}"),
    ("lsquo;", "\u{2018}"),
    ("lt;", "<"),
    ("macr;", "\u{00AF}"),
    ("mdash;", "\u{2014}"),
    ("middot;", "\u{00B7}"),
    ("minus;", "\u{2212}"),
    ("mu;", "\u{03BC}"),
    ("nabla;", "\u{2207}"),
    ("nbsp;", "\u{00A0}"),
    ("ndash;", "\u{2013}"),
    ("ne;", "\u{2260}"),
    ("ni;", "\u{220B}"),
    ("not;", "\u{00AC}"),
    ("notin;", "\u{2209}"),
    ("ntilde;", "\u{00F1}"),
    ("nu;", "\u{03BD}"),
    ("oacute;", "\u{00F3}"),
    ("ocirc;", "\u{00F4}"),
    ("oelig;", "\u{0153}"),
    ("ograve;", "\u{00F2}"),
    ("omega;", "\u{03C9}"),
    ("omicron;", "\u{03BF}"),
    ("oplus;", "\u{2295}"),
    ("or;", "\u{2228}"),
    ("ordf;", "\u{00AA}"),
    ("ordm;", "\u{00BA}"),
    ("oslash;", "\u{00F8}"),
    ("otilde;", "\u{00F5}"),
    ("otimes;", "\u{2297}"),
    ("ouml;", "\u{00F6}"),
    ("para;", "\u{00B6}"),
    ("part;", "\u{2202}"),
    ("perp;", "\u{22A5}"),
    ("phi;", "\u{03C6}"),
    ("pi;", "\u{03C0}"),
    ("plusmn;", "\u{00B1}"),
    ("pound;", "\u{00A3}"),
    ("prime;", "\u{2032}"),
    ("prod;", "\u{220F}"),
    ("prop;", "\u{221D}"),
    ("psi;", "\u{03C8}"),
    ("quot;", "\""),
    ("rArr;", "\u{21D2}"),
    ("radic;", "\u{221A}"),
    ("raquo;", "\u{00BB}"),
    ("rarr;", "\u{2192}"),
    ("rdquo;", "\u{201D}"),
    ("reg;", "\u{00AE}"),
    ("rho;", "\u{03C1}"),
    ("rlm;", "\u{200F}"),
    ("rsaquo;", "\u{203A}"),
    ("rsquo;", "\u{2019}"),
    ("sbquo;", "\u{201A}"),
    ("scaron;", "\u{0161}"),
    ("sdot;", "\u{22C5}"),
    ("sect;", "\u{00A7}"),
    ("shy;", "\u{00AD}"),
    ("sigma;", "\u{03C3}"),
    ("sigmaf;", "\u{03C2}"),
    ("sim;", "\u{223C}"),
    ("spades;", "\u{2660}"),
    ("sub;", "\u{2282}"),
    ("sube;", "\u{2286}"),
    ("sum;", "\u{2211}"),
    ("sup1;", "\u{00B9}"),
    ("sup2;", "\u{00B2}"),
    ("sup3;", "\u{00B3}"),
    ("sup;", "\u{2283}"),
    ("supe;", "\u{2287}"),
    ("szlig;", "\u{00DF}"),
    ("tau;", "\u{03C4}"),
    ("theta;", "\u{03B8}"),
    ("thinsp;", "\u{2009}"),
    ("thorn;", "\u{00FE}"),
    ("tilde;", "\u{02DC}"),
    ("times;", "\u{00D7}"),
    ("trade;", "\u{2122}"),
    ("uArr;", "\u{21D1}"),
    ("uacute;", "\u{00FA}"),
    ("uarr;", "\u{2191}"),
    ("ucirc;", "\u{00FB}"),
    ("ugrave;", "\u{00F9}"),
    ("uml;", "\u{00A8}"),
    ("upsilon;", "\u{03C5}"),
    ("uuml;", "\u{00FC}"),
    ("yacute;", "\u{00FD}"),
    ("yen;", "\u{00A5}"),
    ("yuml;", "\u{00FF}"),
    ("zeta;", "\u{03B6}"),
    ("zwj;", "\u{200D}"),
    ("zwnj;", "\u{200C}"),
];

/// Legacy references valid WITHOUT a trailing `;`, keyed without `;`, sorted.
pub(crate) static LEGACY_ENTITIES: &[(&str, &str)] = &[
    ("AElig", "\u{00C6}"),
    ("Aacute", "\u{00C1}"),
    ("Acirc", "\u{00C2}"),
    ("Agrave", "\u{00C0}"),
    ("Aring", "\u{00C5}"),
    ("Atilde", "\u{00C3}"),
    ("Auml", "\u{00C4}"),
    ("Ccedil", "\u{00C7}"),
    ("ETH", "\u{00D0}"),
    ("Eacute", "\u{00C9}"),
    ("Ecirc", "\u{00CA}"),
    ("Egrave", "\u{00C8}"),
    ("Euml", "\u{00CB}"),
    ("Iacute", "\u{00CD}"),
    ("Icirc", "\u{00CE}"),
    ("Igrave", "\u{00CC}"),
    ("Iuml", "\u{00CF}"),
    ("Ntilde", "\u{00D1}"),
    ("Oacute", "\u{00D3}"),
    ("Ocirc", "\u{00D4}"),
    ("Ograve", "\u{00D2}"),
    ("Oslash", "\u{00D8}"),
    ("Otilde", "\u{00D5}"),
    ("Ouml", "\u{00D6}"),
    ("THORN", "\u{00DE}"),
    ("Uacute", "\u{00DA}"),
    ("Ucirc", "\u{00DB}"),
    ("Ugrave", "\u{00D9}"),
    ("Uuml", "\u{00DC}"),
    ("Yacute", "\u{00DD}"),
    ("aacute", "\u{00E1}"),
    ("acirc", "\u{00E2}"),
    ("acute", "\u{00B4}"),
    ("aelig", "\u{00E6}"),
    ("agrave", "\u{00E0}"),
    ("amp", "&"),
    ("aring", "\u{00E5}"),
    ("atilde", "\u{00E3}"),
    ("auml", "\u{00E4}"),
    ("brvbar", "\u{00A6}"),
    ("ccedil", "\u{00E7}"),
    ("cedil", "\u{00B8}"),
    ("cent", "\u{00A2}"),
    ("copy", "\u{00A9}"),
    ("curren", "\u{00A4}"),
    ("deg", "\u{00B0}"),
    ("divide", "\u{00F7}"),
    ("eacute", "\u{00E9}"),
    ("ecirc", "\u{00EA}"),
    ("egrave", "\u{00E8}"),
    ("eth", "\u{00F0}"),
    ("euml", "\u{00EB}"),
    ("frac12", "\u{00BD}"),
    ("frac14", "\u{00BC}"),
    ("frac34", "\u{00BE}"),
    ("gt", ">"),
    ("iacute", "\u{00ED}"),
    ("icirc", "\u{00EE}"),
    ("iexcl", "\u{00A1}"),
    ("igrave", "\u{00EC}"),
    ("iquest", "\u{00BF}"),
    ("iuml", "\u{00EF}"),
    ("laquo", "\u{00AB}"),
    ("lt", "<"),
    ("macr", "\u{00AF}"),
    ("middot", "\u{00B7}"),
    ("nbsp", "\u{00A0}"),
    ("not", "\u{00AC}"),
    ("ntilde", "\u{00F1}"),
    ("oacute", "\u{00F3}"),
    ("ocirc", "\u{00F4}"),
    ("ograve", "\u{00F2}"),
    ("ordf", "\u{00AA}"),
    ("ordm", "\u{00BA}"),
    ("oslash", "\u{00F8}"),
    ("otilde", "\u{00F5}"),
    ("ouml", "\u{00F6}"),
    ("para", "\u{00B6}"),
    ("plusmn", "\u{00B1}"),
    ("pound", "\u{00A3}"),
    ("raquo", "\u{00BB}"),
    ("reg", "\u{00AE}"),
    ("sect", "\u{00A7}"),
    ("shy", "\u{00AD}"),
    ("sup1", "\u{00B9}"),
    ("sup2", "\u{00B2}"),
    ("sup3", "\u{00B3}"),
    ("szlig", "\u{00DF}"),
    ("thorn", "\u{00FE}"),
    ("times", "\u{00D7}"),
    ("uacute", "\u{00FA}"),
    ("ucirc", "\u{00FB}"),
    ("ugrave", "\u{00F9}"),
    ("uml", "\u{00A8}"),
    ("uuml", "\u{00FC}"),
    ("yacute", "\u{00FD}"),
    ("yen", "\u{00A5}"),
    ("yuml", "\u{00FF}"),
];

/// Maximum entity-name length to scan after `&` (bounds the lookup window).
const MAX_NAME_LEN: usize = 32;

/// Resolve a named/legacy reference. `rest` is the text *after* `&`.
///
/// Returns `(replacement, consumed_len)` where `consumed_len` counts the bytes
/// of the matched name (including a trailing `;` for the named form). `None`
/// when no reference matches.
pub(crate) fn lookup_named(rest: &str) -> Option<(&'static str, usize)> {
    let bytes = rest.as_bytes();
    // Maximal leading run of entity name chars [A-Za-z0-9].
    let run = bytes
        .iter()
        .take(MAX_NAME_LEN)
        .take_while(|b| b.is_ascii_alphanumeric())
        .count();

    // Exact "name;" match if a ';' follows the run within the window.
    if run > 0 && bytes.get(run) == Some(&b';') {
        let key = &rest[..run + 1];
        if let Ok(i) = NAMED_ENTITIES.binary_search_by(|(n, _)| (*n).cmp(key)) {
            return Some((NAMED_ENTITIES[i].1, run + 1));
        }
    }

    // Longest-prefix legacy match over the [A-Za-z] run (digits excluded).
    let alpha_run = bytes
        .iter()
        .take(MAX_NAME_LEN)
        .take_while(|b| b.is_ascii_alphabetic())
        .count();
    for len in (1..=alpha_run).rev() {
        let key = &rest[..len];
        if let Ok(i) = LEGACY_ENTITIES.binary_search_by(|(n, _)| (*n).cmp(key)) {
            return Some((LEGACY_ENTITIES[i].1, len));
        }
    }

    None
}

/// Windows-1252 replacement for C1 control codes `0x80..=0x9F`.
/// Unmapped slots (0x81/0x8D/0x8F/0x90/0x9D) fall through to the codepoint.
pub(crate) fn c1_replacement(code: u32) -> char {
    match code {
        0x80 => '\u{20AC}', // €
        0x82 => '\u{201A}', // ‚
        0x83 => '\u{0192}', // ƒ
        0x84 => '\u{201E}', // „
        0x85 => '\u{2026}', // …
        0x86 => '\u{2020}', // †
        0x87 => '\u{2021}', // ‡
        0x88 => '\u{02C6}', // ˆ
        0x89 => '\u{2030}', // ‰
        0x8A => '\u{0160}', // Š
        0x8B => '\u{2039}', // ‹
        0x8C => '\u{0152}', // Œ
        0x8E => '\u{017D}', // Ž
        0x91 => '\u{2018}', // '
        0x92 => '\u{2019}', // '
        0x93 => '\u{201C}', // "
        0x94 => '\u{201D}', // "
        0x95 => '\u{2022}', // •
        0x96 => '\u{2013}', // –
        0x97 => '\u{2014}', // —
        0x98 => '\u{02DC}', // ˜
        0x99 => '\u{2122}', // ™
        0x9A => '\u{0161}', // š
        0x9B => '\u{203A}', // ›
        0x9C => '\u{0153}', // œ
        0x9E => '\u{017E}', // ž
        0x9F => '\u{0178}', // Ÿ
        _ => char::from_u32(code).unwrap(),
    }
}
