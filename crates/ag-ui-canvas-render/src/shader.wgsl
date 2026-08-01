// Canvas shaders: instanced scene objects + point-cloud blobs.
// One shared camera uniform (group 0); no depth buffer — painter's order.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

// ───── objects: instanced unit quads ─────────────────────────────────────

struct ObjectOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) kind: u32,
};

@vertex
fn vs_object(
    @location(0) corner: vec2<f32>,            // unit quad corner, [-1, 1]
    @location(1) i_pos: vec2<f32>,
    @location(2) i_scale: f32,
    @location(3) i_color: vec4<f32>,
    @location(4) i_kind: u32,
    @location(5) i_pos2: vec2<f32>,            // line end (kind 2)
) -> ObjectOut {
    var out: ObjectOut;
    var world: vec2<f32>;
    if (i_kind == 2u) {
        // Line: lay the unit quad along i_pos -> i_pos2, with half-thickness
        // i_scale across. corner.x spans the length, corner.y the width.
        let dir = i_pos2 - i_pos;
        let len = max(length(dir), 1e-6);
        let axis = dir / len;
        let normal = vec2<f32>(-axis.y, axis.x);
        let t = (corner.x + 1.0) * 0.5;        // 0 at start, 1 at end
        world = i_pos + dir * t + normal * (corner.y * i_scale);
    } else {
        world = i_pos + corner * i_scale;
    }
    out.clip = camera.view_proj * vec4<f32>(world, 0.0, 1.0);
    out.color = i_color;
    out.uv = corner;
    out.kind = i_kind;
    return out;
}

@fragment
fn fs_object(in: ObjectOut) -> @location(0) vec4<f32> {
    // kind 0 renders as a disc; 1 = square, 2 = line (already an oriented quad).
    if (in.kind == 0u && length(in.uv) > 1.0) {
        discard;
    }
    return in.color;
}

// ───── text: instanced glyph quads sampling the ASCII atlas ──────────────

@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

struct TextOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_text(
    @location(0) corner: vec2<f32>,            // unit quad corner, [-1, 1]
    @location(1) i_world: vec2<f32>,           // label anchor (world)
    @location(2) i_size: f32,                  // glyph quad size (world units)
    @location(3) i_offset: vec2<f32>,          // per-glyph offset, in size units
    @location(4) i_uv_min: vec2<f32>,
    @location(5) i_uv_max: vec2<f32>,
    @location(6) i_color: vec4<f32>,
) -> TextOut {
    var out: TextOut;
    let center = i_world + i_offset * i_size;
    let world = center + corner * (i_size * 0.5);
    out.clip = camera.view_proj * vec4<f32>(world, 0.0, 1.0);
    // corner.x: -1 (left) -> uv_min.x, +1 (right) -> uv_max.x.
    // corner.y: +1 (top)  -> uv_min.y, -1 (bottom) -> uv_max.y  (atlas v is down).
    let u = mix(i_uv_min.x, i_uv_max.x, (corner.x + 1.0) * 0.5);
    let v = mix(i_uv_min.y, i_uv_max.y, 1.0 - (corner.y + 1.0) * 0.5);
    out.uv = vec2<f32>(u, v);
    out.color = i_color;
    return out;
}

@fragment
fn fs_text(in: TextOut) -> @location(0) vec4<f32> {
    let cov = textureSample(atlas_tex, atlas_samp, in.uv).r;
    if (cov <= 0.01) {
        discard;
    }
    return vec4<f32>(in.color.rgb, in.color.a * cov);
}

// ───── points: blob vec2<f32> positions as a point list ─────────────────

struct PointOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) color: vec4<f32>,
};

@vertex
fn vs_points(
    @location(0) p: vec2<f32>,
    @location(1) color: vec4<f32>,
) -> PointOut {
    var out: PointOut;
    out.clip = camera.view_proj * vec4<f32>(p, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_points(in: PointOut) -> @location(0) vec4<f32> {
    return in.color;
}
