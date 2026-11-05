//! HTML character references and whitespace.

/// Decodes character references in one pass and collapses runs of whitespace.
///
/// Numeric references (`&#064;`, `&#x2022;`) are all decoded; named references cover the
/// ones that show up in page titles and descriptions. `&amp;#064;` decodes one level only.
/// Unknown references are kept as written.
pub fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(';').map(|j| j + 1).filter(|&j| j <= 12);
        match end.and_then(|j| reference(&rest[1..j]).map(|c| (c, j))) {
            Some((c, j)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    collapse(&out)
}

fn reference(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "middot" => '·',
        "bull" => '•',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "times" => '×',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            return char::from_u32(code).filter(|c| !c.is_control() || c.is_whitespace());
        }
    })
}

/// Trims and turns every run of whitespace into one space.
pub(crate) fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes_named_and_numeric() {
        assert_eq!(
            decode("Tom &amp; Jerry &#8212; &#x41;&hellip;"),
            "Tom & Jerry — A…"
        );
        assert_eq!(decode("a &amp;#064; b"), "a &#064; b");
        assert_eq!(decode("AT&T &unknown; x"), "AT&T &unknown; x");
        assert_eq!(decode("  many\n\t spaces  "), "many spaces");
        assert_eq!(decode("&#0;x"), "&#0;x");
    }
}
