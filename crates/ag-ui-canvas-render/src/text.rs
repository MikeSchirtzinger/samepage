//! ASCII glyph atlas for on-canvas text labels.
//!
//! Rasterizes a vendored monospace font (DejaVu Sans Mono) into a single
//! coverage (`R8`) texture at renderer init, laid out on the SAME grid that
//! `pretext::gpu_layout::GlyphAtlas::default_ascii` describes:
//!
//! - `1024 x 1024` texels, `64 x 64` per cell, `16` cells per row,
//! - ASCII `32..=126`, cell index = `code - 32`.
//!
//! Because the grid matches, the per-glyph UVs that pretext emits in its
//! `GlyphInstance`s index straight into this texture — the renderer never has
//! to know how pretext laid the text out, only how to sample a cell.

use skrifa::{
    instance::{LocationRef, Size},
    outline::{DrawSettings, OutlinePen},
    FontRef, MetadataProvider,
};
use zeno::{Command, Format, Mask, Origin, PathBuilder};

/// Square atlas edge, in texels.
pub const ATLAS_SIZE: u32 = 1024;
/// One glyph cell edge, in texels.
pub const CELL: u32 = 64;
/// Cells per atlas row.
pub const PER_ROW: u32 = ATLAS_SIZE / CELL; // 16
/// First ASCII code in the atlas.
pub const FIRST: u32 = 32;
/// One past the last ASCII code in the atlas (so glyphs are `32..=126`).
pub const LAST: u32 = 127;

/// Em size, in texels, the glyphs are rasterized at. Leaves a little padding
/// inside the 64px cell for ascenders/descenders and side bearings.
const RASTER_PX: f32 = 48.0;

const FONT_BYTES: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");

/// Failure to parse or rasterize the bundled atlas font.
#[derive(Debug)]
pub struct AtlasError(String);

impl std::fmt::Display for AtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AtlasError {}

#[derive(Default)]
struct ZenoPen {
    commands: Vec<Command>,
}

impl OutlinePen for ZenoPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.move_to([x, y]);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.line_to([x, y]);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.commands.quad_to([cx, cy], [x, y]);
    }

    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.commands.curve_to([c0x, c0y], [c1x, c1y], [x, y]);
    }

    fn close(&mut self) {
        self.commands.close();
    }
}

/// Top-left and bottom-right UV of a character's cell, or `None` if the code is
/// outside the atlas range. Mirrors `pretext`'s `GlyphAtlas::char_uvs` for the
/// default ASCII atlas so the two agree by construction.
#[must_use]
pub fn char_uv(code: u32) -> Option<([f32; 2], [f32; 2])> {
    if !(FIRST..LAST).contains(&code) {
        return None;
    }
    let idx = code - FIRST;
    let col = idx % PER_ROW;
    let row = idx / PER_ROW;
    let cell = CELL as f32 / ATLAS_SIZE as f32;
    let uv_min = [col as f32 * cell, row as f32 * cell];
    let [u, v] = uv_min;
    let uv_max = [u + cell, v + cell];
    Some((uv_min, uv_max))
}

/// Build the coverage atlas. Returns a tightly-packed `R8` buffer of
/// `ATLAS_SIZE * ATLAS_SIZE` bytes (one coverage byte per texel) plus the edge
/// length. Each glyph is baseline-aligned within its cell and horizontally
/// centered in the monospace advance box.
pub fn build_atlas_r8() -> Result<(Vec<u8>, u32), AtlasError> {
    let mut pixels = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize];

    let font = FontRef::new(FONT_BYTES)
        .map_err(|error| AtlasError(format!("failed to parse bundled font: {error}")))?;
    let size = Size::new(RASTER_PX);
    let location = LocationRef::default();

    // Baseline row inside a cell. Place it so a full-height glyph (ascent above,
    // descent below) is vertically centered with a little headroom.
    let line = font.metrics(size, location);
    let (ascent, descent) = (line.ascent, line.descent);
    let glyph_h = ascent - descent; // descent is negative
    let cell_f = CELL as f32;
    let baseline = ((cell_f - glyph_h) * 0.5 + ascent).round();

    for code in FIRST..LAST {
        let ch = char::from_u32(code)
            .ok_or_else(|| AtlasError(format!("invalid atlas character code {code}")))?;
        let glyph_id = font
            .charmap()
            .map(ch)
            .ok_or_else(|| AtlasError(format!("bundled font has no glyph for {ch:?}")))?;
        let advance = font
            .glyph_metrics(size, location)
            .advance_width(glyph_id)
            .ok_or_else(|| AtlasError(format!("bundled font has no advance for {ch:?}")))?;
        let Some(glyph) = font.outline_glyphs().get(glyph_id) else {
            continue; // space and the like — leave the cell transparent
        };

        let mut pen = ZenoPen::default();
        glyph
            .draw(DrawSettings::unhinted(size, location), &mut pen)
            .map_err(|error| AtlasError(format!("failed to draw glyph {ch:?}: {error}")))?;

        let mut bitmap = Vec::new();
        let mut mask = Mask::new(&pen.commands);
        mask.format(Format::Alpha).origin(Origin::BottomLeft);
        // `inspect` establishes the dynamic mask dimensions before Zeno uses
        // them to calculate the bottom-left placement returned by render.
        let placement = mask
            .inspect(|format, width, height| {
                bitmap.resize(format.buffer_size(width, height), 0);
            })
            .render_into(&mut bitmap, None);
        if placement.width == 0 || placement.height == 0 {
            continue;
        }

        let idx = code - FIRST;
        let cell_x = ((idx % PER_ROW) * CELL) as i32;
        let cell_y = ((idx / PER_ROW) * CELL) as i32;

        // Center the monospace advance box in the cell, then offset to the
        // glyph's left side bearing.
        let pen_x = (cell_f - advance) * 0.5;
        let glyph_left = (pen_x + placement.left as f32).round() as i32;
        // Zeno's placement top is the distance from the baseline to the top of
        // the bitmap. Convert that to the atlas's cell-local, y-down space.
        let glyph_top = baseline as i32 - placement.top;

        for gy in 0..placement.height as usize {
            let py = cell_y + glyph_top + gy as i32;
            if py < cell_y || py >= cell_y + CELL as i32 {
                continue;
            }
            for gx in 0..placement.width as usize {
                let px = cell_x + glyph_left + gx as i32;
                if px < cell_x || px >= cell_x + CELL as i32 {
                    continue;
                }
                let bitmap_index = gy * placement.width as usize + gx;
                let cov = bitmap.get(bitmap_index).copied().ok_or_else(|| {
                    AtlasError(format!(
                        "rasterizer returned an incomplete bitmap for {ch:?}"
                    ))
                })?;
                if cov == 0 {
                    continue;
                }
                let dst = (py as u32 * ATLAS_SIZE + px as u32) as usize;
                // Glyphs don't overlap within a cell; plain write is fine.
                let pixel = pixels.get_mut(dst).ok_or_else(|| {
                    AtlasError(format!("rasterizer placed {ch:?} outside the atlas"))
                })?;
                *pixel = cov;
            }
        }
    }

    Ok((pixels, ATLAS_SIZE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_has_ink_for_letters_and_none_for_space() {
        let (px, size) = build_atlas_r8().expect("bundled atlas font should rasterize");
        assert_eq!(size, ATLAS_SIZE);
        assert_eq!(px.len(), (ATLAS_SIZE * ATLAS_SIZE) as usize);

        let cell_ink = |code: u32| -> u32 {
            let idx = code - FIRST;
            let cx = (idx % PER_ROW) * CELL;
            let cy = (idx / PER_ROW) * CELL;
            let mut sum = 0u32;
            for y in cy..cy + CELL {
                for x in cx..cx + CELL {
                    sum += u32::from(px[(y * ATLAS_SIZE + x) as usize] > 0);
                }
            }
            sum
        };

        // 'A', 'g', '8' must have coverage; space (32) must be empty.
        assert!(cell_ink(b'A' as u32) > 20, "A should have ink");
        assert!(cell_ink(b'g' as u32) > 20, "g should have ink");
        assert!(cell_ink(b'8' as u32) > 20, "8 should have ink");
        assert_eq!(cell_ink(b' ' as u32), 0, "space should be blank");
    }

    #[test]
    fn char_uv_matches_grid() {
        // 'A' (65) = index 33 → row 2, col 1.
        let (uv_min, uv_max) = char_uv(b'A' as u32).unwrap();
        let cell = CELL as f32 / ATLAS_SIZE as f32;
        assert!((uv_min[0] - cell).abs() < 1e-6);
        assert!((uv_min[1] - 2.0 * cell).abs() < 1e-6);
        assert!((uv_max[0] - 2.0 * cell).abs() < 1e-6);
        assert!(char_uv(0).is_none());
        assert!(char_uv(127).is_none());
    }
}
