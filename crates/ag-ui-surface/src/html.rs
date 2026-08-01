//! The small amount of help a server-rendered surface actually needs.
//!
//! [`RouteBody::Html`](crate::RouteBody::Html) opens a tier where the app
//! author writes only Rust: no build step, no bundle, no toolchain, and no
//! third-party code on the page to sandbox — the server is the only thing that
//! makes markup. That tier has exactly one sharp edge, and it is always the
//! same one: something that came from outside gets interpolated into a string
//! that the browser then parses as HTML.
//!
//! So the escape lives here rather than in each app. Not because it is hard —
//! it is five characters — but because "each app writes its own" is how one of
//! them ends up missing `'` and nobody notices. A model's text, a filename, a
//! form field, and a claim someone typed are all the same kind of input.
//!
//! This is deliberately *not* a template engine. A template engine would be a
//! second way to describe a page, and the point of this tier is that there is
//! one: your own Rust function, returning a `String`.

/// Make text safe to place inside HTML — as element content or inside a quoted
/// attribute value.
///
/// Escapes the five characters that can end the surrounding context: `&`, `<`,
/// `>`, `"`, and `'`. Both quote styles are escaped, so
/// `<div title='{}'>` is as safe as `<div title="{}">`; leaving `'` alone is
/// the usual way a "we escape HTML" helper is quietly wrong.
///
/// It does **not** make text safe inside a `<script>` or `<style>` body, or in
/// an unquoted attribute, or in a `javascript:`/`data:` URL. Those are
/// different grammars and this is not a sanitizer for them — do not reach for
/// this and assume it covered you.
///
/// ```
/// use ag_ui_surface::html::escape;
/// assert_eq!(escape("<b>&\"x\"</b>"), "&lt;b&gt;&amp;&quot;x&quot;&lt;/b&gt;");
/// ```
pub fn escape(value: &str) -> String {
    // Most text contains none of these; only pay for a new allocation's worth
    // of copying when there is something to change.
    if !value
        .bytes()
        .any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\''))
    {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 16);
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_every_context_a_value_could_escape_from() {
        assert_eq!(escape("<script>"), "&lt;script&gt;");
        assert_eq!(escape("a&b"), "a&amp;b");
        // Both quote styles: a helper that escapes only `"` leaves
        // single-quoted attributes injectable, which is the common bug.
        assert_eq!(escape(r#"" onclick=x"#), "&quot; onclick=x");
        assert_eq!(escape("' onclick=x"), "&#39; onclick=x");
    }

    #[test]
    fn an_already_escaped_entity_is_escaped_again_rather_than_trusted() {
        // Idempotence would mean trusting that `&lt;` in the input was ours.
        // It was not; it was data, and data containing the characters `&lt;`
        // must render as those characters.
        assert_eq!(escape("&lt;"), "&amp;lt;");
    }

    #[test]
    fn text_with_nothing_to_escape_survives_unchanged() {
        let plain = "a normal claim, with punctuation — and a dash.";
        assert_eq!(escape(plain), plain);
        assert_eq!(escape(""), "");
        // Non-ASCII passes through; this escapes HTML syntax, not encoding.
        assert_eq!(escape("café ✓ 日本語"), "café ✓ 日本語");
    }
}
