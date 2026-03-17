use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use bytemuck::{Pod, Zeroable};
use eframe::{egui, egui_wgpu, wgpu};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt as _;

use crate::map::osm::OsmTileProvider;
use crate::map::scene::{TileKey, TilePlacement};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SolidVertex {
    pub position: [f32; 3],
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct TexturedVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

#[derive(Clone, Debug, Default)]
pub struct SceneFrame {
    pub tiles: Vec<TileDraw>,
    pub solid_vertices: Vec<SolidVertex>,
    pub solid_indices: Vec<u32>,
    pub view_proj: Mat4,
}

#[derive(Clone, Debug)]
pub struct TileDraw {
    pub texture_key: String,
    pub cache_path: PathBuf,
    pub vertices: [TexturedVertex; 4],
}

#[derive(Clone, Debug)]
pub struct TileLoadEvent {
    pub key: TileKey,
    pub path: PathBuf,
}

pub struct WgpuMapRenderer {
    shared: Arc<Mutex<SharedGpuState>>,
    request_tx: mpsc::Sender<TileKey>,
    result_rx: mpsc::Receiver<TileLoadEvent>,
    inflight: HashSet<TileKey>,
}

struct SharedGpuState {
    target_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    pipelines: Option<Pipelines>,
    textures: HashMap<String, GpuTileTexture>,
}

struct Pipelines {
    camera_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    textured_pipeline: wgpu::RenderPipeline,
    solid_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

struct GpuTileTexture {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

struct PreparedFrame {
    _camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    textured_vertex_buffer: Option<wgpu::Buffer>,
    textured_index_buffer: Option<wgpu::Buffer>,
    tile_draws: Vec<PreparedTileDraw>,
    solid_vertex_buffer: Option<wgpu::Buffer>,
    solid_index_buffer: Option<wgpu::Buffer>,
    solid_index_count: u32,
}

struct PreparedTileDraw {
    texture_key: String,
    index_range: Range<u32>,
}

struct MapPaintCallback {
    shared: Arc<Mutex<SharedGpuState>>,
    scene: SceneFrame,
    prepared: Mutex<Option<PreparedFrame>>,
}

impl WgpuMapRenderer {
    pub fn new(
        render_state: &egui_wgpu::RenderState,
        provider: OsmTileProvider,
        worker_count: usize,
    ) -> Self {
        let shared = Arc::new(Mutex::new(SharedGpuState {
            target_format: render_state.target_format,
            depth_format: egui_wgpu::depth_format_from_bits(24, 0)
                .or(Some(wgpu::TextureFormat::Depth24Plus)),
            pipelines: None,
            textures: HashMap::new(),
        }));

        let (request_tx, request_rx) = mpsc::channel::<TileKey>();
        let (result_tx, result_rx) = mpsc::channel::<TileLoadEvent>();
        let workers = worker_count.max(1);
        Self::spawn_tile_workers(workers, provider, request_rx, result_tx);

        Self {
            shared,
            request_tx,
            result_rx,
            inflight: HashSet::new(),
        }
    }

    fn spawn_tile_workers(
        worker_count: usize,
        provider: OsmTileProvider,
        request_rx: mpsc::Receiver<TileKey>,
        result_tx: mpsc::Sender<TileLoadEvent>,
    ) {
        let request_rx = Arc::new(Mutex::new(request_rx));
        for _ in 0..worker_count {
            let request_rx = Arc::clone(&request_rx);
            let result_tx = result_tx.clone();
            let provider = provider.clone();
            thread::spawn(move || loop {
                let key = {
                    let receiver = request_rx.lock().expect("tile request lock");
                    match receiver.recv() {
                        Ok(key) => key,
                        Err(_) => break,
                    }
                };

                if let Ok(path) = provider.ensure_tile_cached(key.z, key.x, key.y) {
                    let _ = result_tx.send(TileLoadEvent { key, path });
                }
            });
        }
    }

    pub fn request_tiles(&mut self, tiles: &[TilePlacement]) {
        for tile in tiles {
            if tile.cache_path.exists() || self.inflight.contains(&tile.key) {
                continue;
            }
            if self.request_tx.send(tile.key).is_ok() {
                self.inflight.insert(tile.key);
            }
        }
    }

    pub fn drain_tile_events(&mut self) -> usize {
        let mut drained = 0usize;
        while let Ok(event) = self.result_rx.try_recv() {
            self.inflight.remove(&event.key);
            if event.path.exists() {
                drained += 1;
            }
        }
        drained
    }

    pub fn paint_callback(&self, rect: egui::Rect, scene: SceneFrame) -> egui::PaintCallback {
        egui_wgpu::Callback::new_paint_callback(
            rect,
            MapPaintCallback {
                shared: Arc::clone(&self.shared),
                scene,
                prepared: Mutex::new(None),
            },
        )
    }
}

impl SceneFrame {
    pub fn push_tile(&mut self, placement: &TilePlacement) {
        let texture_key = placement.cache_path.to_string_lossy().to_string();
        let vertices = [
            TexturedVertex {
                position: placement.corners[0].to_array(),
                uv: [0.0, 0.0],
            },
            TexturedVertex {
                position: placement.corners[1].to_array(),
                uv: [1.0, 0.0],
            },
            TexturedVertex {
                position: placement.corners[2].to_array(),
                uv: [1.0, 1.0],
            },
            TexturedVertex {
                position: placement.corners[3].to_array(),
                uv: [0.0, 1.0],
            },
        ];
        self.tiles.push(TileDraw {
            texture_key,
            cache_path: placement.cache_path.clone(),
            vertices,
        });
    }

    pub fn push_colored_quad(&mut self, corners: [Vec3; 4], color: [f32; 4]) {
        let start = self.solid_vertices.len() as u32;
        self.solid_vertices
            .extend(corners.map(|corner| SolidVertex {
                position: corner.to_array(),
                color,
            }));
        self.solid_indices.extend_from_slice(&[
            start,
            start + 1,
            start + 2,
            start,
            start + 2,
            start + 3,
        ]);
    }

    pub fn push_marker(&mut self, base: Vec3, size: f32, color: [f32; 4]) {
        let half = size * 0.45;
        let height = size * 1.6;
        let top = base + Vec3::new(0.0, height, 0.0);
        let north = base + Vec3::new(0.0, 0.0, -half);
        let east = base + Vec3::new(half, 0.0, 0.0);
        let south = base + Vec3::new(0.0, 0.0, half);
        let west = base + Vec3::new(-half, 0.0, 0.0);
        let start = self.solid_vertices.len() as u32;
        self.solid_vertices.extend([
            SolidVertex {
                position: top.to_array(),
                color,
            },
            SolidVertex {
                position: north.to_array(),
                color,
            },
            SolidVertex {
                position: east.to_array(),
                color,
            },
            SolidVertex {
                position: south.to_array(),
                color,
            },
            SolidVertex {
                position: west.to_array(),
                color,
            },
        ]);
        self.solid_indices.extend_from_slice(&[
            start,
            start + 1,
            start + 2,
            start,
            start + 2,
            start + 3,
            start,
            start + 3,
            start + 4,
            start,
            start + 4,
            start + 1,
        ]);
    }

    pub fn push_aircraft(&mut self, base: Vec3, size: f32, heading_rad: f32, color: [f32; 4]) {
        let forward = Vec3::new(heading_rad.sin(), 0.0, -heading_rad.cos());
        let right = Vec3::new(forward.z, 0.0, -forward.x);
        let nose = base + forward * size * 1.3 + Vec3::new(0.0, size * 0.35, 0.0);
        let left_wing = base - forward * size * 0.45 - right * size * 0.9;
        let right_wing = base - forward * size * 0.45 + right * size * 0.9;
        let tail = base - forward * size * 1.1;
        let points = [nose, right_wing, tail, left_wing];
        self.push_triangle_fan(&points, color);
    }

    pub fn push_vertical_stem(&mut self, base: Vec3, top: Vec3, radius: f32, color: [f32; 4]) {
        let x = Vec3::new(radius, 0.0, 0.0);
        let z = Vec3::new(0.0, 0.0, radius);
        self.push_colored_quad([base - x, base + x, top + x, top - x], color);
        self.push_colored_quad([base - z, base + z, top + z, top - z], color);
    }

    pub fn push_ground_disc(
        &mut self,
        center: Vec3,
        radius: f32,
        color: [f32; 4],
        segments: usize,
    ) {
        let segments = segments.max(8);
        let mut points = Vec::with_capacity(segments);
        for index in 0..segments {
            let angle = index as f32 / segments as f32 * std::f32::consts::TAU;
            points.push(center + Vec3::new(angle.cos() * radius, 0.0, angle.sin() * radius));
        }
        self.push_triangle_fan(&points, color);
    }

    pub fn push_polyline(&mut self, points: &[Vec3], width: f32, color: [f32; 4]) {
        if points.len() < 2 {
            return;
        }
        for segment in points.windows(2) {
            let a = segment[0];
            let b = segment[1];
            let direction = (b - a).normalize_or_zero();
            let perpendicular = Vec3::new(-direction.z, 0.0, direction.x) * width * 0.5;
            self.push_colored_quad(
                [
                    a - perpendicular,
                    a + perpendicular,
                    b + perpendicular,
                    b - perpendicular,
                ],
                color,
            );
        }
    }

    pub fn push_triangle_fan(&mut self, points: &[Vec3], color: [f32; 4]) {
        if points.len() < 3 {
            return;
        }
        let start = self.solid_vertices.len() as u32;
        self.solid_vertices
            .extend(points.iter().map(|point| SolidVertex {
                position: point.to_array(),
                color,
            }));
        for index in 1..(points.len() as u32 - 1) {
            self.solid_indices
                .extend_from_slice(&[start, start + index, start + index + 1]);
        }
    }
}

impl egui_wgpu::CallbackTrait for MapPaintCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        _callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let mut shared = self.shared.lock().expect("gpu shared state");
        ensure_pipelines(device, &mut shared);
        let (camera_layout, texture_layout, sampler) = {
            let pipelines = shared.pipelines.as_ref().expect("pipelines initialized");
            (
                pipelines.camera_layout.clone(),
                pipelines.texture_layout.clone(),
                pipelines.sampler.clone(),
            )
        };
        for tile in &self.scene.tiles {
            if tile.cache_path.exists() && !shared.textures.contains_key(&tile.texture_key) {
                if let Ok(bytes) = std::fs::read(&tile.cache_path) {
                    if let Ok(image) = image::load_from_memory(&bytes) {
                        let image = image.to_rgba8();
                        let size = image.dimensions();
                        let texture = device.create_texture_with_data(
                            queue,
                            &wgpu::TextureDescriptor {
                                label: Some("vantage-tile"),
                                size: wgpu::Extent3d {
                                    width: size.0,
                                    height: size.1,
                                    depth_or_array_layers: 1,
                                },
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::COPY_DST,
                                view_formats: &[],
                            },
                            wgpu::util::TextureDataOrder::LayerMajor,
                            image.as_raw(),
                        );
                        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("vantage-tile-bind-group"),
                            layout: &texture_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(&view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::Sampler(&sampler),
                                },
                            ],
                        });
                        shared.textures.insert(
                            tile.texture_key.clone(),
                            GpuTileTexture {
                                _texture: texture,
                                _view: view,
                                bind_group,
                            },
                        );
                    }
                }
            }
        }

        let camera_uniform = CameraUniform {
            view_proj: self.scene.view_proj.to_cols_array_2d(),
        };
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vantage-camera-uniform"),
            contents: bytemuck::bytes_of(&camera_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vantage-camera-bind-group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let mut textured_vertices = Vec::with_capacity(self.scene.tiles.len() * 4);
        let mut textured_indices = Vec::with_capacity(self.scene.tiles.len() * 6);
        let mut tile_draws = Vec::new();
        for tile in &self.scene.tiles {
            if !shared.textures.contains_key(&tile.texture_key) {
                continue;
            }
            let start_vertex = textured_vertices.len() as u32;
            let start_index = textured_indices.len() as u32;
            textured_vertices.extend_from_slice(&tile.vertices);
            textured_indices.extend_from_slice(&[
                start_vertex,
                start_vertex + 1,
                start_vertex + 2,
                start_vertex,
                start_vertex + 2,
                start_vertex + 3,
            ]);
            tile_draws.push(PreparedTileDraw {
                texture_key: tile.texture_key.clone(),
                index_range: start_index..start_index + 6,
            });
        }

        let textured_vertex_buffer = (!textured_vertices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vantage-tile-vertex-buffer"),
                contents: bytemuck::cast_slice(&textured_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let textured_index_buffer = (!textured_indices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vantage-tile-index-buffer"),
                contents: bytemuck::cast_slice(&textured_indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        });

        let solid_vertex_buffer = (!self.scene.solid_vertices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vantage-solid-vertex-buffer"),
                contents: bytemuck::cast_slice(&self.scene.solid_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let solid_index_buffer = (!self.scene.solid_indices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vantage-solid-index-buffer"),
                contents: bytemuck::cast_slice(&self.scene.solid_indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        });

        *self.prepared.lock().expect("prepared frame") = Some(PreparedFrame {
            _camera_buffer: camera_buffer,
            camera_bind_group,
            textured_vertex_buffer,
            textured_index_buffer,
            tile_draws,
            solid_vertex_buffer,
            solid_index_buffer,
            solid_index_count: self.scene.solid_indices.len() as u32,
        });

        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        _callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let shared = self.shared.lock().expect("gpu shared state");
        let Some(pipelines) = shared.pipelines.as_ref() else {
            return;
        };
        let prepared = self.prepared.lock().expect("prepared frame");
        let Some(prepared) = prepared.as_ref() else {
            return;
        };

        let clip = info.clip_rect_in_pixels();
        render_pass.set_scissor_rect(
            clip.left_px.max(0) as u32,
            clip.from_bottom_px.max(0) as u32,
            clip.width_px.max(0) as u32,
            clip.height_px.max(0) as u32,
        );

        if let (Some(vertex_buffer), Some(index_buffer)) = (
            prepared.textured_vertex_buffer.as_ref(),
            prepared.textured_index_buffer.as_ref(),
        ) {
            render_pass.set_pipeline(&pipelines.textured_pipeline);
            render_pass.set_bind_group(0, &prepared.camera_bind_group, &[]);
            render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            render_pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            for draw in &prepared.tile_draws {
                if let Some(texture) = shared.textures.get(&draw.texture_key) {
                    render_pass.set_bind_group(1, &texture.bind_group, &[]);
                    render_pass.draw_indexed(draw.index_range.clone(), 0, 0..1);
                }
            }
        }

        if let (Some(vertex_buffer), Some(index_buffer)) = (
            prepared.solid_vertex_buffer.as_ref(),
            prepared.solid_index_buffer.as_ref(),
        ) {
            render_pass.set_pipeline(&pipelines.solid_pipeline);
            render_pass.set_bind_group(0, &prepared.camera_bind_group, &[]);
            render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            render_pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..prepared.solid_index_count, 0, 0..1);
        }
    }
}

fn ensure_pipelines(device: &wgpu::Device, shared: &mut SharedGpuState) {
    if shared.pipelines.is_some() {
        return;
    }

    let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("vantage-camera-layout"),
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
    let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("vantage-texture-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
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
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("vantage-tile-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Nearest,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });

    let textured_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vantage-textured-shader"),
        source: wgpu::ShaderSource::Wgsl(TEXTURED_SHADER.into()),
    });
    let textured_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("vantage-textured-pipeline-layout"),
        bind_group_layouts: &[&camera_layout, &texture_layout],
        push_constant_ranges: &[],
    });
    let textured_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("vantage-textured-pipeline"),
        layout: Some(&textured_layout),
        vertex: wgpu::VertexState {
            module: &textured_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<TexturedVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2],
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &textured_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: shared.target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: shared.depth_format.map(|format| wgpu::DepthStencilState {
            format,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::LessEqual,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
        cache: None,
    });

    let solid_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vantage-solid-shader"),
        source: wgpu::ShaderSource::Wgsl(SOLID_SHADER.into()),
    });
    let solid_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("vantage-solid-pipeline-layout"),
        bind_group_layouts: &[&camera_layout],
        push_constant_ranges: &[],
    });
    let solid_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("vantage-solid-pipeline"),
        layout: Some(&solid_layout),
        vertex: wgpu::VertexState {
            module: &solid_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<SolidVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &solid_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: shared.target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: shared.depth_format.map(|format| wgpu::DepthStencilState {
            format,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::LessEqual,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
        cache: None,
    });

    shared.pipelines = Some(Pipelines {
        camera_layout,
        texture_layout,
        textured_pipeline,
        solid_pipeline,
        sampler,
    });
}

const TEXTURED_SHADER: &str = r#"
struct CameraUniform {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var tile_texture: texture_2d<f32>;
@group(1) @binding(1) var tile_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = camera.view_proj * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(tile_texture, tile_sampler, input.uv);
    return vec4<f32>(color.rgb, 1.0);
}
"#;

const SOLID_SHADER: &str = r#"
struct CameraUniform {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = camera.view_proj * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
"#;
