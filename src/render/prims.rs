//! One instanced SDF shader for rects, rounded rects, borders and dots.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

/// A primitive in physical pixels; colours are linear with straight alpha.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Prim {
    pub rect: [f32; 4],
    pub color: [f32; 4],
    pub border_color: [f32; 4],
    /// x: corner radius, y: border width.
    pub params: [f32; 4],
}

impl Prim {
    pub fn rect(rect: [f32; 4], color: [f32; 4]) -> Self {
        Self {
            rect,
            color,
            border_color: [0.0; 4],
            params: [0.0; 4],
        }
    }

    pub fn rounded(rect: [f32; 4], color: [f32; 4], radius: f32) -> Self {
        Self {
            params: [radius, 0.0, 0.0, 0.0],
            ..Self::rect(rect, color)
        }
    }

    /// An outline only.
    pub fn outline(rect: [f32; 4], color: [f32; 4], radius: f32, width: f32) -> Self {
        Self {
            rect,
            color: [0.0; 4],
            border_color: color,
            params: [radius, width, 0.0, 0.0],
        }
    }

    /// A dot centred on `c`, optionally ringed.
    pub fn dot(c: [f32; 2], r: f32, color: [f32; 4], ring: Option<([f32; 4], f32)>) -> Self {
        let (border_color, w) = ring.unwrap_or(([0.0; 4], 0.0));
        let r = r + w;
        Self {
            rect: [c[0] - r, c[1] - r, 2.0 * r, 2.0 * r],
            color,
            border_color,
            params: [r, w, 0.0, 0.0],
        }
    }
}

pub struct PrimPipeline {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
    /// Instances of the under layer, then the over layer.
    split: u32,
    count: u32,
}

const ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4];

fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("prims"),
        size: (capacity * size_of::<Prim>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl PrimPipeline {
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let globals = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("prim globals"),
            contents: bytemuck::bytes_of(&[0.0f32; 4]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("prims"),
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
            label: Some("prims"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/prims.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("prims"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("prims"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Prim>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &ATTRS,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let capacity = 64;
        Self {
            pipeline,
            globals,
            bind_group,
            instances: instance_buffer(device, capacity),
            capacity,
            split: 0,
            count: 0,
        }
    }

    /// Upload both layers: `under` draws before images and thumbnails, `over` after.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        under: &[Prim],
        over: &[Prim],
        viewport: [f32; 2],
    ) {
        let prims: Vec<Prim> = under.iter().chain(over).copied().collect();
        queue.write_buffer(
            &self.globals,
            0,
            bytemuck::bytes_of(&[viewport[0], viewport[1], 0.0, 0.0]),
        );
        if prims.len() > self.capacity {
            self.capacity = prims.len().next_power_of_two();
            self.instances = instance_buffer(device, self.capacity);
        }
        if !prims.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&prims));
        }
        self.split = under.len() as u32;
        self.count = prims.len() as u32;
    }

    fn draw_range(&self, pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, range);
    }

    pub fn draw_under(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_range(pass, 0..self.split);
    }

    pub fn draw_over(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_range(pass, self.split..self.count);
    }
}
