//! wgpu renderer for the AG-UI shared canvas.
//!
//! Pure wgpu — no `web-sys`, no `wasm-bindgen` — so the exact same render
//! code runs headless on native (the correctness gate: render-to-PNG tests)
//! and against a browser canvas on wasm32. The browser glue crate constructs
//! the [`wgpu::SurfaceTarget`] and passes it in.
//!
//! Buffer discipline (the binary-channel payoff):
//! - **Instance buffer** (scene objects) re-uploads only when the scene
//!   changed (`set_objects` marks dirty), never per frame.
//! - **Blob buffers** (point clouds) upload once per `(blob_id, generation)`
//!   — `upload_blob` takes the aligned `&[u8]` payload straight off the wire
//!   and hands it to `Queue::write_buffer`. No parse, no repack.

use std::collections::HashMap;
use std::fmt;

use glam::{Mat4, Vec2};

pub mod text;

pub use wgpu; // re-export so callers name SurfaceTarget etc. from one place

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobUploadError {
    InvalidLength { bytes: usize },
    TooManyVertices { vertices: usize },
}

impl fmt::Display for BlobUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength { bytes } => write!(
                formatter,
                "point-cloud payload length {bytes} is not a multiple of 8 bytes"
            ),
            Self::TooManyVertices { vertices } => write!(
                formatter,
                "point-cloud payload contains {vertices} vertices, exceeding u32"
            ),
        }
    }
}

impl std::error::Error for BlobUploadError {}

fn blob_vertex_count(payload: &[u8]) -> Result<u32, BlobUploadError> {
    if !payload.len().is_multiple_of(8) {
        return Err(BlobUploadError::InvalidLength {
            bytes: payload.len(),
        });
    }
    let vertices = payload.len() / 8;
    u32::try_from(vertices).map_err(|_| BlobUploadError::TooManyVertices { vertices })
}

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.043,
    g: 0.051,
    b: 0.07,
    a: 1.0,
};

/// Renderer-facing mirror of one CRDT scene object (the web crate maps
/// `ag_ui_canvas::ObjectSnapshot` into this).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneObject {
    pub x: f32,
    pub y: f32,
    /// Second endpoint for line objects (kind 2); equals (x,y) otherwise.
    pub x2: f32,
    pub y2: f32,
    /// Disc/square: radius/half-size. Line: half-thickness.
    pub scale: f32,
    /// Packed RGBA (0xRRGGBBAA).
    pub color: u32,
    /// 0 = disc, 1 = square, 2 = line (shader-side).
    pub kind: u32,
}

/// Per-instance GPU layout. Packed with no padding so field offsets match
/// `vertex_attr_array!`'s sequential offsets (0, 8, 12, 28, 32; stride 40) —
/// keep in lockstep with `shader.wgsl` locations 1-5.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    pos: [f32; 2],
    scale: f32,
    color: [f32; 4],
    kind: u32,
    pos2: [f32; 2],
}

fn color_components(color: u32) -> [f32; 4] {
    let [r, g, b, a] = color.to_be_bytes();
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

impl From<&SceneObject> for Instance {
    fn from(o: &SceneObject) -> Self {
        Instance {
            pos: [o.x, o.y],
            scale: o.scale,
            color: color_components(o.color),
            kind: o.kind,
            pos2: [o.x2, o.y2],
        }
    }
}

/// One glyph quad to draw as on-canvas text, in world space.
///
/// This is the renderer's text-instance contract — the browser glue maps
/// `pretext::gpu_layout::GlyphInstance` (its label layout output) into this.
/// `offset` follows pretext's center-of-advance convention: the glyph is drawn
/// centered at `world_pos + offset * size`, in a `size`-by-`size` quad. `uv_*`
/// index the ASCII atlas (see [`text`]). World-space sizing means labels scale
/// with zoom, staying pinned to their world anchor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextQuad {
    /// World-space anchor the whole label is positioned from.
    pub world_pos: [f32; 2],
    /// World-space height (and width) of one glyph quad.
    pub size: f32,
    /// Per-glyph offset from the anchor, in `size` units (center-of-advance).
    pub offset: [f32; 2],
    /// Atlas UV of the glyph cell's top-left.
    pub uv_min: [f32; 2],
    /// Atlas UV of the glyph cell's bottom-right.
    pub uv_max: [f32; 2],
    /// RGBA tint (0..1).
    pub color: [f32; 4],
}

/// Per-instance GPU layout for one text glyph. Tightly packed f32s (stride 52)
/// — keep in lockstep with `shader.wgsl` `vs_text` locations 1-6.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextInstance {
    world_pos: [f32; 2],
    size: f32,
    offset: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: [f32; 4],
}

impl From<&TextQuad> for TextInstance {
    fn from(q: &TextQuad) -> Self {
        TextInstance {
            world_pos: q.world_pos,
            size: q.size,
            offset: q.offset,
            uv_min: q.uv_min,
            uv_max: q.uv_max,
            color: q.color,
        }
    }
}

/// 2D pan/zoom camera. `zoom` is pixels per world unit.
#[derive(Debug, Clone, Copy)]
pub struct Camera2D {
    pub center: Vec2,
    pub zoom: f32,
    viewport: (f32, f32),
}

impl Camera2D {
    fn new(width: u32, height: u32) -> Self {
        Self {
            center: Vec2::ZERO,
            zoom: 1.0,
            viewport: (width as f32, height as f32),
        }
    }

    fn view_proj(&self) -> Mat4 {
        let half_w = self.viewport.0 / (2.0 * self.zoom);
        let half_h = self.viewport.1 / (2.0 * self.zoom);
        Mat4::orthographic_rh(
            self.center.x - half_w,
            self.center.x + half_w,
            self.center.y - half_h,
            self.center.y + half_h,
            -1.0,
            1.0,
        )
    }

    /// Screen pixels (y down, origin top-left) → world coordinates (y up).
    pub fn screen_to_world(&self, sx: f32, sy: f32) -> (f32, f32) {
        let wx = self.center.x + (sx - self.viewport.0 / 2.0) / self.zoom;
        let wy = self.center.y - (sy - self.viewport.1 / 2.0) / self.zoom;
        (wx, wy)
    }

    /// World coordinates (y up) → screen pixels (y down, origin top-left).
    /// Inverse of [`screen_to_world`](Self::screen_to_world); the bridge for
    /// any DOM/overlay UI that needs to track a world anchor on pan/zoom.
    pub fn world_to_screen(&self, wx: f32, wy: f32) -> (f32, f32) {
        let sx = (wx - self.center.x) * self.zoom + self.viewport.0 / 2.0;
        let sy = (self.center.y - wy) * self.zoom + self.viewport.1 / 2.0;
        (sx, sy)
    }
}

struct BlobGpu {
    buffer: wgpu::Buffer,
    /// One uniform RGBA value supplied as a step-per-instance vertex stream.
    /// Keeping it beside the geometry lets each cloud use its scene color
    /// without repacking the Nx2 position blob.
    color_buffer: wgpu::Buffer,
    generation: u32,
    capacity: u64,
    vertex_count: u32,
}

enum RenderTarget {
    Surface {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    Headless {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
    },
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: RenderTarget,
    format: wgpu::TextureFormat,
    size: (u32, u32),

    objects_pipeline: wgpu::RenderPipeline,
    points_pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,
    camera_bind_group: wgpu::BindGroup,
    atlas_bind_group: wgpu::BindGroup,

    quad_buffer: wgpu::Buffer,
    camera_buffer: wgpu::Buffer,

    instances: Vec<Instance>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
    instances_dirty: bool,

    text_instances: Vec<TextInstance>,
    text_buffer: wgpu::Buffer,
    text_capacity: u64,
    text_dirty: bool,

    blobs: HashMap<u64, BlobGpu>,

    pub camera: Camera2D,
    camera_dirty: bool,
}

#[derive(Debug)]
pub enum RendererError {
    Adapter(String),
    Device(String),
    Surface(String),
    Atlas(String),
}

impl std::fmt::Display for RendererError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RendererError::Adapter(e) => write!(f, "no suitable GPU adapter: {e}"),
            RendererError::Device(e) => write!(f, "device request failed: {e}"),
            RendererError::Surface(e) => write!(f, "surface creation failed: {e}"),
            RendererError::Atlas(e) => write!(f, "text atlas creation failed: {e}"),
        }
    }
}

impl std::error::Error for RendererError {}

impl Renderer {
    /// Browser path: render into a canvas-backed surface built by the caller.
    pub async fn new_with_surface(
        instance: &wgpu::Instance,
        target: wgpu::SurfaceTarget<'static>,
        width: u32,
        height: u32,
    ) -> Result<Self, RendererError> {
        let surface = instance
            .create_surface(target)
            .map_err(|e| RendererError::Surface(e.to_string()))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| RendererError::Adapter(e.to_string()))?;
        let (device, queue) = request_device(&adapter).await?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .first()
            .copied()
            .ok_or_else(|| RendererError::Surface("no surface formats".into()))?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        Self::common(
            device,
            queue,
            RenderTarget::Surface { surface, config },
            format,
            (width, height),
        )
    }

    /// Native path: render into an offscreen texture (tests, tooling).
    pub async fn new_headless(width: u32, height: u32) -> Result<Self, RendererError> {
        let instance = default_instance();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| RendererError::Adapter(e.to_string()))?;
        let (device, queue) = request_device(&adapter).await?;

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = create_offscreen(&device, format, width, height);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Self::common(
            device,
            queue,
            RenderTarget::Headless { texture, view },
            format,
            (width, height),
        )
    }

    fn common(
        device: wgpu::Device,
        queue: wgpu::Queue,
        target: RenderTarget,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> Result<Self, RendererError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: std::mem::size_of::<Mat4>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            immediate_size: 0,
        });

        // Unit quad, triangle strip.
        let quad: [[f32; 2]; 4] = [[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]];
        let quad_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unit-quad"),
            size: std::mem::size_of_val(&quad) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&quad_buffer, 0, bytemuck::cast_slice(&quad));

        let instance_capacity = 64 * std::mem::size_of::<Instance>() as u64;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: instance_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let blend_target = [Some(wgpu::ColorTargetState {
            format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let objects_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("objects"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_object"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            1 => Float32x2,  // pos
                            2 => Float32,    // scale
                            3 => Float32x4,  // color
                            4 => Uint32,     // kind
                            5 => Float32x2,  // pos2 (line end)
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_object"),
                compilation_options: Default::default(),
                targets: &blend_target,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let points_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("points"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_points"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<[f32; 4]>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![1 => Float32x4],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_points"),
                compilation_options: Default::default(),
                targets: &blend_target,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::PointList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ── text atlas + pipeline ────────────────────────────────────────
        let (atlas_pixels, atlas_size) =
            text::build_atlas_r8().map_err(|error| RendererError::Atlas(error.to_string()))?;
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("text-atlas"),
            size: wgpu::Extent3d {
                width: atlas_size,
                height: atlas_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas_pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas_size), // R8: 1 byte/texel, 1024 % 256 == 0
                rows_per_image: Some(atlas_size),
            },
            wgpu::Extent3d {
                width: atlas_size,
                height: atlas_size,
                depth_or_array_layers: 1,
            },
        );
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("text-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text-atlas-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text-atlas-bind"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                },
            ],
        });

        let text_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });
        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text"),
            layout: Some(&text_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_text"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<TextInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            1 => Float32x2, // world_pos
                            2 => Float32,   // size
                            3 => Float32x2, // offset
                            4 => Float32x2, // uv_min
                            5 => Float32x2, // uv_max
                            6 => Float32x4, // color
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_text"),
                compilation_options: Default::default(),
                targets: &blend_target,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let text_capacity = 256 * std::mem::size_of::<TextInstance>() as u64;
        let text_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text-instances"),
            size: text_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            device,
            queue,
            target,
            format,
            size,
            objects_pipeline,
            points_pipeline,
            text_pipeline,
            camera_bind_group,
            atlas_bind_group,
            quad_buffer,
            camera_buffer,
            instances: Vec::new(),
            instance_buffer,
            instance_capacity,
            instances_dirty: false,
            text_instances: Vec::new(),
            text_buffer,
            text_capacity,
            text_dirty: false,
            blobs: HashMap::new(),
            camera: Camera2D::new(size.0, size.1),
            camera_dirty: true,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == self.size {
            return;
        }
        self.size = (width, height);
        self.camera.viewport = (width as f32, height as f32);
        self.camera_dirty = true;
        match &mut self.target {
            RenderTarget::Surface { surface, config } => {
                config.width = width;
                config.height = height;
                surface.configure(&self.device, config);
            }
            RenderTarget::Headless { texture, view } => {
                *texture = create_offscreen(&self.device, self.format, width, height);
                *view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            }
        }
    }

    // ── camera ───────────────────────────────────────────────────────────

    pub fn pan(&mut self, dx_px: f32, dy_px: f32) {
        self.camera.center.x -= dx_px / self.camera.zoom;
        self.camera.center.y += dy_px / self.camera.zoom;
        self.camera_dirty = true;
    }

    /// Frame a world-space rectangle: center the camera on it and pick the
    /// zoom that fits both axes inside the viewport with ~12% padding. Used to
    /// auto-fit freshly-arrived content so it fills the view instead of sitting
    /// as a speck at the default zoom. A degenerate (near-zero) extent falls
    /// back to a sane minimum span so a single point doesn't zoom to infinity.
    pub fn frame_bounds(&mut self, min_x: f32, min_y: f32, max_x: f32, max_y: f32) {
        const MIN_SPAN: f32 = 4.0;
        const PAD: f32 = 1.24; // 12% margin on each side
        let span_x = (max_x - min_x).abs().max(MIN_SPAN) * PAD;
        let span_y = (max_y - min_y).abs().max(MIN_SPAN) * PAD;
        let (vw, vh) = self.camera.viewport;
        if vw <= 0.0 || vh <= 0.0 {
            return;
        }
        let zoom = (vw / span_x).min(vh / span_y).clamp(0.05, 100.0);
        self.camera.center = Vec2::new((min_x + max_x) * 0.5, (min_y + max_y) * 0.5);
        self.camera.zoom = zoom;
        self.camera_dirty = true;
    }

    /// Zoom by `factor`, keeping the world point under screen (cx, cy) fixed.
    pub fn zoom_at(&mut self, factor: f32, cx: f32, cy: f32) {
        let before = self.camera.screen_to_world(cx, cy);
        self.camera.zoom = (self.camera.zoom * factor).clamp(0.05, 100.0);
        let after = self.camera.screen_to_world(cx, cy);
        self.camera.center.x += before.0 - after.0;
        self.camera.center.y += before.1 - after.1;
        self.camera_dirty = true;
    }

    pub fn screen_to_world(&self, sx: f32, sy: f32) -> (f32, f32) {
        self.camera.screen_to_world(sx, sy)
    }

    pub fn world_to_screen(&self, wx: f32, wy: f32) -> (f32, f32) {
        self.camera.world_to_screen(wx, wy)
    }

    // ── scene data ───────────────────────────────────────────────────────

    /// Replace the object instance list (call after each CRDT apply). Cheap
    /// for the demo's object counts; upload happens lazily in `render`.
    pub fn set_objects(&mut self, objects: &[SceneObject]) {
        self.instances.clear();
        self.instances.extend(objects.iter().map(Instance::from));
        self.instances_dirty = true;
    }

    /// Replace the on-canvas text glyphs (one [`TextQuad`] per glyph). The
    /// browser glue rebuilds these from the scene's `label` objects via
    /// `pretext`'s layout engine and calls this whenever labels change.
    pub fn set_text_quads(&mut self, quads: &[TextQuad]) {
        self.text_instances.clear();
        self.text_instances
            .extend(quads.iter().map(TextInstance::from));
        self.text_dirty = true;
    }

    /// Upload a point-cloud blob payload (`vec2<f32>` positions, the aligned
    /// slice straight off the wire) using the historical light-blue default.
    /// New scene-backed callers should use [`Self::upload_blob_colored`] so
    /// the requested cloud color reaches the framebuffer.
    pub fn upload_blob(
        &mut self,
        blob_id: u64,
        generation: u32,
        payload: &[u8],
    ) -> Result<(), BlobUploadError> {
        self.upload_blob_colored(blob_id, generation, payload, 0x73D4FFFF)
    }

    /// Upload point-cloud geometry and its packed `0xRRGGBBAA` scene color.
    /// Geometry is generation-gated, while color is refreshed even when the
    /// payload generation is unchanged so an ordinary CRDT recolor is visible
    /// without needlessly resending a large blob.
    pub fn upload_blob_colored(
        &mut self,
        blob_id: u64,
        generation: u32,
        payload: &[u8],
        color: u32,
    ) -> Result<(), BlobUploadError> {
        let vertex_count = blob_vertex_count(payload)?;
        let rgba = color_components(color);
        if let Some(existing) = self.blobs.get(&blob_id) {
            self.queue
                .write_buffer(&existing.color_buffer, 0, bytemuck::bytes_of(&rgba));
            if existing.generation >= generation {
                return Ok(());
            }
        }
        let needed = payload.len() as u64;

        let new_blob = || {
            let capacity = needed.next_power_of_two().max(1024);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("blob"),
                size: capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let color_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("blob-color"),
                size: std::mem::size_of::<[f32; 4]>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            BlobGpu {
                buffer,
                color_buffer,
                generation,
                capacity,
                vertex_count,
            }
        };
        let blob = match self.blobs.entry(blob_id) {
            std::collections::hash_map::Entry::Occupied(mut entry)
                if entry.get().capacity < needed =>
            {
                entry.insert(new_blob());
                entry.into_mut()
            }
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(new_blob()),
        };
        blob.generation = generation;
        blob.vertex_count = vertex_count;
        self.queue.write_buffer(&blob.buffer, 0, payload);
        self.queue
            .write_buffer(&blob.color_buffer, 0, bytemuck::bytes_of(&rgba));
        Ok(())
    }

    /// Update one resident cloud's color from a newer scene snapshot. Returns
    /// whether the blob is currently resident (it may arrive later on the
    /// binary channel during reconnect, in which case upload supplies color).
    pub fn set_blob_color(&mut self, blob_id: u64, color: u32) -> bool {
        let Some(blob) = self.blobs.get(&blob_id) else {
            return false;
        };
        let rgba = color_components(color);
        self.queue
            .write_buffer(&blob.color_buffer, 0, bytemuck::bytes_of(&rgba));
        true
    }

    /// Keep only blobs referenced by the authoritative scene snapshot. This
    /// is the clear/reconnect path: CRDT object deletion removes stale GPU
    /// buffers even if no separate blob tombstone exists on the wire.
    pub fn retain_blobs(&mut self, active_blob_ids: &[u64]) {
        self.blobs
            .retain(|blob_id, _| active_blob_ids.contains(blob_id));
    }

    pub fn drop_blob(&mut self, blob_id: u64) {
        self.blobs.remove(&blob_id);
    }

    // ── rendering ────────────────────────────────────────────────────────

    fn flush_dirty(&mut self) {
        if self.camera_dirty {
            let m = self.camera.view_proj();
            self.queue
                .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&m));
            self.camera_dirty = false;
        }
        if self.instances_dirty {
            let needed = (self.instances.len() * std::mem::size_of::<Instance>()) as u64;
            if needed > self.instance_capacity {
                self.instance_capacity = needed.next_power_of_two();
                self.instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("instances"),
                    size: self.instance_capacity,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            if !self.instances.is_empty() {
                self.queue.write_buffer(
                    &self.instance_buffer,
                    0,
                    bytemuck::cast_slice(&self.instances),
                );
            }
            self.instances_dirty = false;
        }
        if self.text_dirty {
            let needed = (self.text_instances.len() * std::mem::size_of::<TextInstance>()) as u64;
            if needed > self.text_capacity {
                self.text_capacity = needed.next_power_of_two();
                self.text_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("text-instances"),
                    size: self.text_capacity,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            if !self.text_instances.is_empty() {
                self.queue.write_buffer(
                    &self.text_buffer,
                    0,
                    bytemuck::cast_slice(&self.text_instances),
                );
            }
            self.text_dirty = false;
        }
    }

    fn encode_pass(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("canvas-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_bind_group(0, &self.camera_bind_group, &[]);

        // Point clouds first (under the objects).
        pass.set_pipeline(&self.points_pipeline);
        for blob in self.blobs.values() {
            if blob.vertex_count > 0 {
                pass.set_vertex_buffer(0, blob.buffer.slice(..));
                pass.set_vertex_buffer(1, blob.color_buffer.slice(..));
                pass.draw(0..blob.vertex_count, 0..1);
            }
        }

        if !self.instances.is_empty() {
            pass.set_pipeline(&self.objects_pipeline);
            pass.set_vertex_buffer(0, self.quad_buffer.slice(..));
            pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
            pass.draw(0..4, 0..self.instances.len() as u32);
        }

        // Text labels last, so they sit on top of discs and clouds.
        if !self.text_instances.is_empty() {
            pass.set_pipeline(&self.text_pipeline);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            pass.set_vertex_buffer(0, self.quad_buffer.slice(..));
            pass.set_vertex_buffer(1, self.text_buffer.slice(..));
            pass.draw(0..4, 0..self.text_instances.len() as u32);
        }
    }

    /// Render one frame. On a surface target, acquires/presents the
    /// swapchain frame (reconfiguring once on Lost/Outdated).
    pub fn render(&mut self) {
        self.flush_dirty();
        match &self.target {
            RenderTarget::Surface { surface, config } => {
                use wgpu::CurrentSurfaceTexture as Cst;
                let frame = match surface.get_current_texture() {
                    Cst::Success(frame) => frame,
                    Cst::Suboptimal(frame) => {
                        // Usable, but reconfigure for the next frame.
                        surface.configure(&self.device, config);
                        frame
                    }
                    Cst::Outdated | Cst::Lost => {
                        surface.configure(&self.device, config);
                        match surface.get_current_texture() {
                            Cst::Success(frame) | Cst::Suboptimal(frame) => frame,
                            other => {
                                log::warn!("skipping frame after reconfigure: {other:?}");
                                return;
                            }
                        }
                    }
                    other => {
                        log::warn!("skipping frame: {other:?}");
                        return;
                    }
                };
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder = self
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                self.encode_pass(&mut encoder, &view);
                self.queue.submit([encoder.finish()]);
                frame.present();
            }
            RenderTarget::Headless { view, .. } => {
                let mut encoder = self
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                self.encode_pass(&mut encoder, view);
                self.queue.submit([encoder.finish()]);
            }
        }
    }

    /// Headless only, native only: render and read back tightly-packed RGBA8
    /// pixels. The blocking poll makes this unusable on wasm.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_to_pixels(&mut self) -> Option<(Vec<u8>, u32, u32)> {
        self.flush_dirty();
        let RenderTarget::Headless { texture, view } = &self.target else {
            return None;
        };

        let (width, height) = self.size;
        // COPY_BYTES_PER_ROW_ALIGNMENT (256) padding for the readback copy.
        let unpadded = width * 4;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.encode_pass(&mut encoder, view);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;

        let mapped = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((unpadded * height) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            let end = start + unpadded as usize;
            let Some(row_pixels) = mapped.get(start..end) else {
                drop(mapped);
                readback.unmap();
                return None;
            };
            pixels.extend_from_slice(row_pixels);
        }
        drop(mapped);
        readback.unmap();
        Some((pixels, width, height))
    }
}

async fn request_device(
    adapter: &wgpu::Adapter,
) -> Result<(wgpu::Device, wgpu::Queue), RendererError> {
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("canvas-device"),
            ..Default::default()
        })
        .await
        .map_err(|e| RendererError::Device(e.to_string()))
}

/// Instance with the right backend per target (Metal/Vulkan/DX12 native,
/// WebGPU in the browser).
pub fn default_instance() -> wgpu::Instance {
    let backends = if cfg!(target_arch = "wasm32") {
        wgpu::Backends::BROWSER_WEBGPU
    } else {
        wgpu::Backends::PRIMARY
    };
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

fn create_offscreen(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_upload_rejects_trailing_partial_vertex() {
        assert_eq!(
            blob_vertex_count(&[0; 7]),
            Err(BlobUploadError::InvalidLength { bytes: 7 })
        );
        assert_eq!(blob_vertex_count(&[0; 16]), Ok(2));
    }
}
