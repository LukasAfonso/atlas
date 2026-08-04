use std::{ops::Range, sync::Arc};

use bytemuck::{Pod, Zeroable};
use eframe::{
    CreationContext,
    egui::{Color32, Painter, Rect},
    egui_wgpu::{
        Callback, CallbackResources, CallbackTrait, ScreenDescriptor,
        wgpu::{self, util::DeviceExt as _},
    },
};

use super::{BoardState, Camera, layout::ClusterRegion, render::cluster_color};

const SHADER: &str = r"
struct Camera {
    transform: vec4<f32>,
    viewport: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

fn unpack_color(color: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(color & 255u),
        f32((color >> 8u) & 255u),
        f32((color >> 16u) & 255u),
        f32((color >> 24u) & 255u),
    ) / 255.0;
}

@vertex
fn vs_main(@location(0) world: vec2<f32>, @location(1) color: u32) -> VertexOutput {
    let center = camera.transform.xy;
    let scale = camera.transform.z;
    let half_size = camera.viewport.xy;
    let offset = (world - center) * scale / half_size;
    var out: VertexOutput;
    out.position = vec4<f32>(offset.x, -offset.y, 0.0, 1.0);
    out.color = unpack_color(color);
    return out;
}

fn linear_from_gamma_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let cutoff = rgb < vec3<f32>(0.04045);
    let lower = rgb / vec3<f32>(12.92);
    let higher = pow((rgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

@fragment
fn fs_gamma(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}

@fragment
fn fs_linear(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(linear_from_gamma_rgb(in.color.rgb), in.color.a);
}
";

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
struct ClusterVertex {
    position: [f32; 2],
    color: u32,
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
struct CameraUniform {
    transform: [f32; 4],
    viewport: [f32; 4],
}

#[derive(Debug)]
struct ClusterDraw {
    bounds: Rect,
    indices: Range<u32>,
}

#[derive(Debug, Default)]
pub(super) struct ClusterMesh {
    vertices: Vec<ClusterVertex>,
    indices: Vec<u32>,
    draws: Vec<ClusterDraw>,
}

impl ClusterMesh {
    pub(super) fn from_clusters(clusters: &[ClusterRegion]) -> Self {
        let vertex_count = clusters
            .iter()
            .map(|cluster| cluster.geometry.vertices.len())
            .sum();
        let index_count = clusters
            .iter()
            .map(|cluster| cluster.geometry.indices.len())
            .sum();
        let mut mesh = Self {
            vertices: Vec::with_capacity(vertex_count),
            indices: Vec::with_capacity(index_count),
            draws: Vec::with_capacity(clusters.len()),
        };

        for cluster in clusters {
            let vertex_offset = mesh.vertices.len() as u32;
            let index_start = mesh.indices.len() as u32;
            let color = cluster_fill(cluster_color(&cluster.key));
            mesh.vertices.extend(
                cluster
                    .geometry
                    .vertices
                    .iter()
                    .map(|position| ClusterVertex {
                        position: [position.x, position.y],
                        color,
                    }),
            );
            mesh.indices.extend(
                cluster
                    .geometry
                    .indices
                    .iter()
                    .map(|index| vertex_offset + index),
            );
            mesh.draws.push(ClusterDraw {
                bounds: cluster.bounds(),
                indices: index_start..mesh.indices.len() as u32,
            });
        }
        mesh
    }

    fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

fn cluster_fill(color: Color32) -> u32 {
    u32::from_le_bytes(
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24).to_array(),
    )
}

struct GpuMesh {
    source: Arc<ClusterMesh>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
}

struct ClusterResources {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    mesh: Option<GpuMesh>,
}

impl ClusterResources {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("atlas_cluster_shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atlas_cluster_camera"),
            contents: bytemuck::bytes_of(&CameraUniform::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas_cluster_bind_group_layout"),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas_cluster_bind_group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("atlas_cluster_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("atlas_cluster_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ClusterVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Uint32],
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(if target_format.is_srgb() {
                    "fs_linear"
                } else {
                    "fs_gamma"
                }),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            uniform,
            bind_group,
            mesh: None,
        }
    }

    fn install(&mut self, device: &wgpu::Device, source: Arc<ClusterMesh>) {
        if self
            .mesh
            .as_ref()
            .is_some_and(|mesh| Arc::ptr_eq(&mesh.source, &source))
        {
            return;
        }
        self.mesh = Some(GpuMesh {
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atlas_cluster_vertices"),
                contents: bytemuck::cast_slice(&source.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atlas_cluster_indices"),
                contents: bytemuck::cast_slice(&source.indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
            source,
        });
    }
}

pub(crate) fn initialize(creation_context: &CreationContext<'_>) {
    let render_state = creation_context
        .wgpu_render_state
        .as_ref()
        .expect("Atlas requires the wgpu renderer");
    let resources = ClusterResources::new(&render_state.device, render_state.target_format);
    render_state
        .renderer
        .write()
        .callback_resources
        .insert(resources);
}

struct ClusterCallback {
    mesh: Arc<ClusterMesh>,
    camera: CameraUniform,
    visible: Vec<Range<u32>>,
}

impl CallbackTrait for ClusterCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let resources = resources
            .get_mut::<ClusterResources>()
            .expect("cluster renderer was not initialized");
        resources.install(device, Arc::clone(&self.mesh));
        queue.write_buffer(&resources.uniform, 0, bytemuck::bytes_of(&self.camera));
        Vec::new()
    }

    fn paint(
        &self,
        _info: eframe::egui::epaint::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let resources = resources
            .get::<ClusterResources>()
            .expect("cluster renderer was not initialized");
        let Some(mesh) = resources.mesh.as_ref() else {
            return;
        };
        render_pass.set_pipeline(&resources.pipeline);
        render_pass.set_bind_group(0, &resources.bind_group, &[]);
        render_pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        render_pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        for indices in &self.visible {
            render_pass.draw_indexed(indices.clone(), 0, 0..1);
        }
    }
}

impl BoardState {
    pub(super) fn paint_cluster_fill(&self, painter: &Painter, viewport: Rect) {
        if self.cluster_mesh.is_empty() {
            return;
        }
        let visible = self
            .cluster_mesh
            .draws
            .iter()
            .filter(|draw| cluster_is_visible(self.camera, draw.bounds, viewport))
            .map(|draw| draw.indices.clone())
            .collect();
        let callback = ClusterCallback {
            mesh: Arc::clone(&self.cluster_mesh),
            camera: CameraUniform {
                transform: [
                    self.camera.center.x,
                    self.camera.center.y,
                    self.camera.scale,
                    0.0,
                ],
                viewport: [
                    (viewport.width() * 0.5).max(0.5),
                    (viewport.height() * 0.5).max(0.5),
                    0.0,
                    0.0,
                ],
            },
            visible,
        };
        painter.add(Callback::new_paint_callback(viewport, callback));
    }
}

fn cluster_is_visible(camera: Camera, bounds: Rect, viewport: Rect) -> bool {
    let screen_bounds = Rect::from_min_max(
        camera.world_to_screen(bounds.min, viewport),
        camera.world_to_screen(bounds.max, viewport),
    );
    viewport.intersects(screen_bounds)
}

#[cfg(test)]
mod tests {
    use super::cluster_fill;
    use eframe::egui::Color32;

    #[test]
    fn cluster_fill_keeps_the_existing_translucent_color() {
        assert_eq!(
            cluster_fill(Color32::from_rgb(10, 20, 30)).to_le_bytes(),
            Color32::from_rgba_unmultiplied(10, 20, 30, 24).to_array()
        );
    }
}
