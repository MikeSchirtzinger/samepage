//! Small, dependency-free helpers used by every extractor: a SHA-256
//! implementation (no crypto crate is on the allowed-dependency list for this
//! pass), a line-comment stripper, and a brace-depth scanner used to tell a
//! top-level statement from one buried inside a nested block.

/// SHA-256 of a byte slice, returned as a lowercase hex string.
///
/// Implemented directly (FIPS 180-4) instead of pulling in a crypto crate,
/// since evidence hashing is the only place this crate needs a hash and the
/// dependency budget for this pass is deliberately small.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    h.iter().map(|word| format!("{word:08x}")).collect()
}

/// Trims a source line for evidence display: leading/trailing whitespace
/// removed, then capped at 200 chars (the cap lands on a char boundary).
pub fn trimmed_snippet(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= 200 {
        trimmed.to_string()
    } else {
        trimmed.chars().take(200).collect()
    }
}

/// Strips a `//` line comment from a line of Rust or JS/TS source, so a
/// pattern written only in prose doesn't get picked up as a real call site.
///
/// Heuristic, not a lexer: it tracks single/double-quoted strings (with
/// backslash escapes) so `"http://example.com"` doesn't get truncated at the
/// first slash, but it does not understand raw strings (`r"..."`, `r#"..."#`)
/// or block comments (`/* ... */`); a `//` inside either of those is treated
/// as a real comment start. Good enough for the line-oriented matching this
/// crate does; a real answer needs a lexer, which this first pass does not
/// carry.
pub fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match in_str {
            Some(q) => {
                if b == b'\\' {
                    i += 1; // skip the escaped char too
                } else if b == q {
                    in_str = None;
                }
            }
            None => {
                if b == b'"' || b == b'\'' {
                    in_str = Some(b);
                } else if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
                    return &line[..i];
                }
            }
        }
        i += 1;
    }
    line
}

/// Byte ranges, within a file's source, that a `#[cfg(test)]`-annotated item
/// spans (from the attribute itself through the matching close brace of the
/// item it decorates). Matches inside these ranges are test code, not a real
/// lane, and are dropped rather than reported.
///
/// Only handles the common `#[cfg(test)]` spelling directly above a `mod` or
/// `fn`; it does not resolve `#[cfg(any(test, ...))]` or attributes reached
/// through a macro.
pub fn cfg_test_ranges(source: &str) -> Vec<(usize, usize)> {
    let attr_re = regex::Regex::new(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]").unwrap();
    let mut ranges = Vec::new();
    for m in attr_re.find_iter(source) {
        let after = &source[m.end()..];
        // Find the first `{` after the attribute; that opens the item body.
        if let Some(brace_offset) = after.find('{') {
            let body_start = m.end() + brace_offset;
            if let Some(body_end) = matching_brace_end(source, body_start) {
                ranges.push((m.start(), body_end));
            }
        }
    }
    ranges
}

/// Given the byte offset of an opening `{`, returns the byte offset just
/// past its matching `}` (depth-counted, comment- and string-aware at the
/// same fidelity as [`strip_line_comment`]).
pub fn matching_brace_end(source: &str, open_brace_offset: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    debug_assert_eq!(bytes.get(open_brace_offset), Some(&b'{'));
    let mut depth: i64 = 0;
    let mut in_str: Option<u8> = None;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut i = open_brace_offset;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
        } else if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
                i += 1;
            }
        } else if let Some(q) = in_str {
            if b == b'\\' {
                i += 1;
            } else if b == q {
                in_str = None;
            }
        } else {
            match b {
                b'"' | b'\'' => in_str = Some(b),
                b'/' if next == Some(b'/') => {
                    in_line_comment = true;
                    i += 1;
                }
                b'/' if next == Some(b'*') => {
                    in_block_comment = true;
                    i += 1;
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// A function span found by [`fn_spans`]: its name, the byte offset of its
/// opening `{`, and the byte offset just past its matching `}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnSpan {
    pub name: String,
    pub body_start: usize,
    pub body_end: usize,
    /// True if the line directly above `fn` (skipping blank lines) carries
    /// `#[tokio::main]`, so this function is an async entry point even when
    /// it isn't literally named `main`.
    pub is_tokio_main: bool,
}

/// Finds every `fn <name>(...) { ... }` in Rust source, with enough context
/// (name, body span, `#[tokio::main]` marker) to decide whether a spawn call
/// inside it sits at the function's own top level.
///
/// Regex-based, not a parser: it does not resolve generics-heavy or macro
/// generated signatures, and a `fn` appearing inside a string or comment
/// would be a false positive in principle, though none of the extractors in
/// this crate look far enough past a match to hit that in practice.
pub fn fn_spans(source: &str) -> Vec<FnSpan> {
    let fn_re = regex::Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^>]*>)?\s*\(").unwrap();
    let mut spans = Vec::new();
    for cap in fn_re.captures_iter(source) {
        let whole = cap.get(0).unwrap();
        let name = cap[1].to_string();
        // Skip past the parameter list, honoring nested parens.
        let after_paren = &source[whole.end()..];
        let mut depth = 1i64;
        let mut idx = 0usize;
        let bytes = after_paren.as_bytes();
        while idx < bytes.len() && depth > 0 {
            match bytes[idx] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            idx += 1;
        }
        if depth != 0 {
            continue; // unbalanced; give up on this candidate
        }
        let after_params = whole.end() + idx;
        // From here to the opening `{` may contain a return type / where
        // clause; skip to the first `{` or `;` (a trait method with no
        // body).
        let tail = &source[after_params..];
        let Some(brace_rel) = tail.find(['{', ';']) else {
            continue;
        };
        if tail.as_bytes()[brace_rel] != b'{' {
            continue; // declaration only, e.g. a trait method signature
        }
        let body_start = after_params + brace_rel;
        let Some(body_end) = matching_brace_end(source, body_start) else {
            continue;
        };

        // Look at whole lines strictly *before* the one `fn` sits on — not
        // `source[..whole.start()]` directly, since that also contains this
        // same line's own prefix (e.g. `async `), which would derail the
        // `take_while` before it ever reached a `#[tokio::main]` line above.
        let fn_line_start = source[..whole.start()]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let preceding = &source[..fn_line_start];
        let is_tokio_main = preceding
            .lines()
            .rev()
            .take_while(|l| {
                let t = l.trim();
                t.is_empty() || t.starts_with('#') || t.starts_with("pub")
            })
            .any(|l| l.trim().starts_with("#[tokio::main"));

        spans.push(FnSpan {
            name,
            body_start,
            body_end,
            is_tokio_main,
        });
    }
    spans
}

/// The local brace depth, relative to a function's own body, at a given
/// byte offset inside that body. `0` means the offset is a direct statement
/// of the function (its own top level); `1`+ means it's nested inside an
/// `if`/`loop`/`match`/closure block within that function.
pub fn local_depth_at(source: &str, body_start: usize, offset: usize) -> i64 {
    scan_depth(source, body_start + 1, offset)
}

/// The brace depth at a byte offset, counted from the start of the file.
/// `0` means the offset sits outside every `{}` block — module top level
/// for a JS/TS file with no wrapping function.
pub fn depth_at(source: &str, offset: usize) -> i64 {
    scan_depth(source, 0, offset)
}

/// Shared brace-depth scanner: counts net `{`/`}` between `start` and `end`,
/// skipping over string contents and comments at the same fidelity as
/// [`strip_line_comment`] and [`matching_brace_end`].
fn scan_depth(source: &str, start: usize, end: usize) -> i64 {
    let bytes = source.as_bytes();
    let mut depth: i64 = 0;
    let mut in_str: Option<u8> = None;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut i = start;
    while i < bytes.len() && i < end {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
        } else if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
                i += 1;
            }
        } else if let Some(q) = in_str {
            if b == b'\\' {
                i += 1;
            } else if b == q {
                in_str = None;
            }
        } else {
            match b {
                b'"' | b'\'' => in_str = Some(b),
                b'/' if next == Some(b'/') => {
                    in_line_comment = true;
                    i += 1;
                }
                b'/' if next == Some(b'*') => {
                    in_block_comment = true;
                    i += 1;
                }
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
        }
        i += 1;
    }
    depth
}

/// Returns the full line of text (no trailing newline) that contains the
/// given byte offset.
pub fn line_at(source: &str, offset: usize) -> &str {
    let offset = offset.min(source.len());
    let start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = source[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(source.len());
    source[start..end].trim_end_matches('\r')
}

/// Converts a byte offset into a 1-based line number.
pub fn line_number_at(source: &str, offset: usize) -> u32 {
    (source[..offset.min(source.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn strips_a_trailing_comment_but_not_a_url_in_a_string() {
        assert_eq!(
            strip_line_comment(r#"let url = "http://x"; // TcpListener::bind"#),
            r#"let url = "http://x"; "#
        );
    }

    #[test]
    fn recognizes_tokio_main_despite_the_async_keyword_on_the_same_line() {
        let src = "#[tokio::main]\nasync fn main() {\n    let _x = 1;\n}\n";
        let spans = fn_spans(src);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "main");
        assert!(spans[0].is_tokio_main);
    }

    #[test]
    fn finds_cfg_test_module_range() {
        let src = "fn main() {}\n#[cfg(test)]\nmod tests {\n    fn a() {}\n}\n";
        let ranges = cfg_test_ranges(src);
        assert_eq!(ranges.len(), 1);
        let (start, end) = ranges[0];
        assert!(src[start..end].contains("mod tests"));
        assert!(src[start..end].contains("fn a"));
    }
}
