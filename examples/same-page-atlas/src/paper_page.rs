//! The tier-1 page: markup the server makes, and nothing else.
//!
//! This is the whole client. There is no module to import, no bundle to build,
//! no wasm to compile, and no framework — one vendored file and a `<form>`.
//! `cargo run` and it works, which is the entire point of the tier: somebody
//! who wants a working UI in front of them *now*, on a machine whose toolchain
//! they would rather not think about.
//!
//! Everything here is a constant because there is nothing to interpolate. The
//! parts that vary are fragments, rendered in [`crate::paper`] and swapped in
//! by htmx, and every value that reaches them goes through
//! `ag_ui_surface::html::escape` first.
//!
//! It renders the same `Board` the browser-module view renders. Neither knows
//! the other exists, and the vocabulary they share has no idea either — which
//! is the claim `crate::assertion` opens with, now standing up rather than
//! asserted.

/// The shell. `hx-get` polls; the server answers `204` when the caller already
/// has the current revision, and htmx skips the swap on 204.
///
/// That is not an optimisation. Re-swapping the whole board on a timer destroys
/// and recreates every control roughly as fast as a person can move a pointer
/// toward one, so clicks land on elements that no longer exist and the surface
/// feels broken. Swapping only on a real change is what makes it clickable.
pub const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="dark">
<title>The board, on paper · Same Page Atlas</title>
<!--
  htmx 4.0.0-beta6, vendored and pinned. Nothing on this page loads from a
  remote host. The exact bytes are asserted by `vendored_htmx_is_pinned`;
  replacing this file without updating that test fails the build.
-->
<script src="/htmx.min.js"></script>
<link rel="stylesheet" href="/paper.css">
</head>
<body>

<header class="top">
  <div class="brand"><span class="dot"></span><span>The board, on paper</span></div>
  <p class="sub">
    Server-rendered. No build step, no bundle, no JavaScript written by hand —
    and the same board the <a href="/">main page</a> shows, in the same process,
    over the same actions.
  </p>
  <nav class="tabs">
    <button class="tab active" data-view="board" type="button">Review</button>
    <button class="tab" data-view="primitives" type="button">Primitives</button>
    <button class="seen-btn"
            hx-post="/board/paper/seen"
            hx-target="#board"
            hx-swap="innerHTML"
            title="Clear the &quot;new since you looked&quot; count">mark seen</button>
  </nav>
</header>

<main>
  <section id="view-board" class="view">
    <div id="board"
         hx-get="/board/paper/rows"
         hx-trigger="load, every 1s"
         hx-vals='js:{since: document.querySelector("#board .c-rev")?.dataset.rev ?? ""}'
         hx-swap="innerHTML"></div>

    <form class="compose"
          method="post"
          action="/board/paper/compose"
          hx-post="/board/paper/compose"
          hx-target="#board"
          hx-swap="innerHTML"
          hx-on::after-request="this.reset()">
      <select name="kind" aria-label="kind">
        <option value="claim">claim</option>
        <option value="question">question</option>
      </select>
      <input name="text" placeholder="Correct me, or ask something — it will be signed &quot;you&quot;" autocomplete="off" required>
      <button type="submit">add</button>
    </form>
  </section>

  <section id="view-primitives" class="view" hidden>
    <p class="lede">
      Every assertion the board can hold, generated from the Rust enum that
      parses them — so this cannot drift from what the host actually accepts.
    </p>
    <div id="primitives" hx-get="/board/paper/primitives" hx-trigger="load" hx-swap="innerHTML"></div>
  </section>
</main>

<script>
  // The only hand-written script on the page, and it does one thing: switch
  // tabs. Everything that touches the board goes through htmx and the server.
  document.querySelectorAll('.tab').forEach(function (tab) {
    tab.addEventListener('click', function () {
      var view = tab.dataset.view;
      document.querySelectorAll('.tab').forEach(function (t) { t.classList.toggle('active', t === tab); });
      document.getElementById('view-board').hidden = view !== 'board';
      document.getElementById('view-primitives').hidden = view !== 'primitives';
    });
  });
</script>

</body>
</html>"##;

/// The vendored front-end dependency, pinned by content rather than by a
/// version string in a comment.
///
/// htmx 4 is a **beta** (`4.0.0-beta6`; npm's `latest` tag is still 2.0.10),
/// so "the version we tested against" and "the version on disk" have to be the
/// same fact or the pin is decorative. Swapping the file — an upgrade, a
/// half-finished download, anything — fails this test with the new digest to
/// paste in, which is the moment to re-read the changelog.
#[cfg(test)]
mod pinned {
    /// sha256 of `static/htmx.min.js`, htmx.org@4.0.0-beta6 `dist/htmx.min.js`.
    const HTMX_SHA256: &str = "28fae7bbe8e8142b702debb9d5234a9a436d9435a4b5165b195aa1a7ed840d25";
    const HTMX_VERSION: &str = "4.0.0-beta6";

    /// A tiny sha256, so pinning a 36 KB file does not cost the workspace a
    /// dependency it would otherwise never take.
    fn sha256(data: &[u8]) -> String {
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
        let mut message = data.to_vec();
        let bit_len = (data.len() as u64) * 8;
        message.push(0x80);
        while message.len() % 64 != 56 {
            message.push(0);
        }
        message.extend_from_slice(&bit_len.to_be_bytes());

        for chunk in message.chunks(64) {
            let mut w = [0u32; 64];
            for (i, word) in chunk.chunks(4).enumerate() {
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
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *slot = slot.wrapping_add(value);
            }
        }
        h.iter().map(|word| format!("{word:08x}")).collect()
    }

    #[test]
    fn vendored_htmx_is_pinned() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/static/htmx.min.js");
        let bytes = std::fs::read(path).expect("the vendored htmx bundle is missing");
        let actual = sha256(&bytes);
        assert_eq!(
            actual, HTMX_SHA256,
            "static/htmx.min.js is not the pinned htmx {HTMX_VERSION} bundle.\n\
             If this was a deliberate upgrade, read the changelog first, then set\n\
             HTMX_SHA256 to: {actual}"
        );
    }

    #[test]
    fn the_page_loads_only_the_pinned_bundle() {
        // A remote <script> would silently defeat the pin — and would also
        // defeat the tier's own claim, which is that nothing third-party
        // arrives on this page at all.
        let html = crate::paper_page::PAGE;
        assert!(
            html.contains(r#"<script src="/htmx.min.js"></script>"#),
            "the page must load the vendored bundle"
        );
        assert!(
            !html.contains("//unpkg.com") && !html.contains("//cdn.") && !html.contains("https://"),
            "the page must not reach a remote host for anything"
        );
    }

    #[test]
    fn compose_identity_survives_an_ordinary_form_post() {
        let html = crate::paper_page::PAGE;
        assert!(html.contains(r#"method="post""#));
        assert!(html.contains(r#"action="/board/paper/compose""#));
        assert!(
            !html.contains("agui_human_route_session"),
            "the HttpOnly caller credential must not be copied into markup"
        );
    }
}
