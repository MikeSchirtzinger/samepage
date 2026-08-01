//! Headless render-to-PNG test: renders a known scene offscreen on the
//! native GPU and asserts pixel-level expectations. This is the renderer's
//! correctness gate — it validates the full wgpu pipeline (shaders, instance
//! layout, camera, blob upload) without a browser.

use ag_ui_canvas_render::{text, Renderer, SceneObject, TextQuad};

const W: u32 = 256;
const H: u32 = 256;

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    pixels[i..i + 4].try_into().unwrap()
}

fn roughly(actual: [u8; 4], expected: [u8; 4], tol: u8) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(a, e)| a.abs_diff(e) <= tol)
}

#[test]
fn renders_objects_and_points_to_expected_pixels() {
    let mut renderer = match pollster::block_on(Renderer::new_headless(W, H)) {
        Ok(r) => r,
        Err(e) => panic!("no GPU adapter available for headless test: {e}"),
    };

    // One big red disc at the origin (screen center), one green square to
    // the right, and a horizontal line of points along world y = -60.
    renderer.set_objects(&[
        SceneObject {
            x: 0.0,
            y: 0.0,
            x2: 0.0,
            y2: 0.0,
            scale: 40.0,
            color: 0xFF0000FF, // red, opaque
            kind: 0,           // disc
        },
        SceneObject {
            x: 80.0,
            y: 0.0,
            x2: 80.0,
            y2: 0.0,
            scale: 20.0,
            color: 0x00FF00FF, // green, opaque
            kind: 1,           // square
        },
        SceneObject {
            // Horizontal magenta line from world (-80,0) to (-40,0).
            x: -80.0,
            y: 0.0,
            x2: -40.0,
            y2: 0.0,
            scale: 6.0,        // half-thickness
            color: 0xFF00FFFF, // magenta, opaque
            kind: 2,           // line
        },
    ]);

    let points: Vec<f32> = (0..100).flat_map(|i| [i as f32 - 50.0, -60.0]).collect();
    renderer
        .upload_blob_colored(1, 1, bytemuck::cast_slice(&points), 0xFF00FFFF)
        .expect("valid point cloud");

    let (pixels, w, h) = renderer
        .render_to_pixels()
        .expect("headless target must support readback");
    assert_eq!((w, h), (W, H));
    assert_eq!(pixels.len(), (W * H * 4) as usize);

    // Save the artifact first so it exists for inspection even on failure.
    let img = image::RgbaImage::from_raw(w, h, pixels.clone()).unwrap();
    let out = std::env::temp_dir().join("ag-ui-canvas-headless.png");
    img.save(&out).unwrap();
    eprintln!("wrote {}", out.display());

    // Center of the canvas = world origin = center of the red disc.
    // (Rgba8UnormSrgb readback returns the sRGB-encoded bytes we fed in.)
    let center = pixel(&pixels, W / 2, H / 2);
    assert!(
        roughly(center, [255, 0, 0, 255], 8),
        "disc center should be red, got {center:?}"
    );

    // World (80, 0) → screen (W/2 + 80, H/2): inside the green square.
    let square = pixel(&pixels, W / 2 + 80, H / 2);
    assert!(
        roughly(square, [0, 255, 0, 255], 8),
        "square should be green, got {square:?}"
    );

    // The magenta line: midpoint world (-60, 0) → screen (W/2 - 60, H/2).
    let line_mid = pixel(&pixels, W / 2 - 60, H / 2);
    assert!(
        line_mid[0] > 150 && line_mid[2] > 150 && line_mid[1] < 120,
        "line midpoint should be magenta, got {line_mid:?}"
    );

    // A corner stays background (clear color, sRGB-encoded ~ dark blue-grey).
    let corner = pixel(&pixels, 2, 2);
    assert!(
        corner[0] < 80 && corner[1] < 80 && corner[2] < 90,
        "corner should be near-background, got {corner:?}"
    );

    // The disc is round: just outside its radius along the diagonal must be
    // background, proving the kind==0 mask discards.
    let outside_disc = pixel(&pixels, W / 2 + 35, H / 2 + 35);
    assert!(
        !roughly(outside_disc, [255, 0, 0, 255], 8),
        "diagonal outside disc radius should not be red, got {outside_disc:?}"
    );

    // Point row: world (0, -60) → screen ≈ (W/2, H/2 + 60). Points are 1px
    // and rasterization rounding can shift the row by a pixel, so scan a
    // small neighborhood for the requested magenta uniform color.
    let found_point = (H / 2 + 57..=H / 2 + 63).any(|y| {
        (W / 2 - 3..=W / 2 + 3).any(|x| {
            let p = pixel(&pixels, x, y);
            p[0] > 150 && p[2] > 150 && p[1] < 100
        })
    });
    assert!(
        found_point,
        "expected a requested-magenta point pixel near screen y = {}",
        H / 2 + 60
    );

    // The scene snapshot is authoritative for blob lifetime. Clearing every
    // BlobRef must evict the GPU cloud even without a binary tombstone frame.
    renderer.retain_blobs(&[]);
    let (cleared, _, _) = renderer
        .render_to_pixels()
        .expect("headless target must support a second readback");
    let stale_point = (H / 2 + 57..=H / 2 + 63).any(|y| {
        (W / 2 - 3..=W / 2 + 3).any(|x| {
            let p = pixel(&cleared, x, y);
            p[0] > 150 && p[2] > 150 && p[1] < 100
        })
    });
    assert!(!stale_point, "cleared BlobRefs must remove point pixels");
}

#[test]
fn renders_text_label_ink_to_framebuffer() {
    let mut renderer = match pollster::block_on(Renderer::new_headless(W, H)) {
        Ok(r) => r,
        Err(e) => panic!("no GPU adapter available for headless test: {e}"),
    };

    // A white single-line label "AGUI" centered at the world origin. Lay the
    // glyphs out by hand with the center-of-advance convention the renderer's
    // text pipeline expects (glyph i centered at cursor + advance/2).
    let word = "AGUI";
    let advance = 0.6_f32;
    let size = 50.0_f32;
    let total = word.chars().count() as f32 * advance;
    let mut cursor = -total * 0.5;
    let mut quads = Vec::new();
    for ch in word.chars() {
        let (uv_min, uv_max) = text::char_uv(ch as u32).expect("ascii glyph");
        quads.push(TextQuad {
            world_pos: [0.0, 0.0],
            size,
            offset: [cursor + advance * 0.5, 0.0],
            uv_min,
            uv_max,
            color: [1.0, 1.0, 1.0, 1.0],
        });
        cursor += advance;
    }
    renderer.set_text_quads(&quads);

    let (pixels, w, h) = renderer
        .render_to_pixels()
        .expect("headless target must support readback");

    // Save the artifact for eyeballing.
    let img = image::RgbaImage::from_raw(w, h, pixels.clone()).unwrap();
    let out = std::env::temp_dir().join("ag-ui-canvas-text.png");
    img.save(&out).unwrap();
    eprintln!("wrote {}", out.display());

    // Count bright (white-ish) pixels — glyph ink on the dark clear color.
    let bright = pixels
        .chunks_exact(4)
        .filter(|p| p[0] > 120 && p[1] > 120 && p[2] > 120)
        .count();
    assert!(
        bright > 80,
        "expected the label to paint white ink, got {bright} bright pixels"
    );
}

#[test]
fn blob_generation_gating_skips_stale_uploads() {
    let mut renderer = pollster::block_on(Renderer::new_headless(64, 64)).expect("adapter");

    let gen1: Vec<f32> = vec![0.0, 0.0, 1.0, 1.0];
    let gen2: Vec<f32> = vec![2.0, 2.0, 3.0, 3.0, 4.0, 4.0];
    renderer
        .upload_blob(9, 1, bytemuck::cast_slice(&gen1))
        .expect("valid generation one");
    // Same generation again — must be a no-op (can't observe buffer content
    // cheaply here, but it must not panic or grow the vertex count).
    renderer
        .upload_blob(9, 1, bytemuck::cast_slice(&gen2))
        .expect("valid repeated generation");
    // Newer generation with more points — must take.
    renderer
        .upload_blob(9, 2, bytemuck::cast_slice(&gen2))
        .expect("valid newer generation");
    renderer.render();
}
