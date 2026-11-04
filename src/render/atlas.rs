//! Thumbnail atlas: square slots in texture pages, evicted least-recently-drawn first.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::thumbs::Thumb;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Preferred page side; smaller if the GPU can't do it, larger for huge cells.
const PAGE: u32 = 2048;

/// A thumbnail to draw: `rect` in physical pixels.
pub struct ThumbDraw<'a> {
    pub path: &'a Path,
    pub rect: [f32; 4],
    pub alpha: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    rect: [f32; 4],
    uv: [f32; 4],
    params: [f32; 4],
}

const ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];

struct Page {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

#[derive(Clone, Copy)]
struct Slot {
    page: usize,
    index: u32,
    size: [u32; 2],
    used: u64,
}

pub struct Atlas {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    page_layout: wgpu::BindGroupLayout,
    instances: wgpu::Buffer,
    capacity: usize,
    /// Per page: instance range drawn this frame.
    batches: Vec<(usize, std::ops::Range<u32>)>,
    max_dim: u32,
    page_side: u32,
    /// Slot side: the cell plus a 1 px transparent border all round.
    slot: u32,
    pages: Vec<Page>,
    slots: HashMap<PathBuf, Slot>,
    free: Vec<(usize, u32)>,
    frame: u64,
}

fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("thumb instances"),
        size: (capacity * size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl Atlas {
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat, max_dim: u32) -> Self {
        let globals = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("thumb globals"),
            contents: bytemuck::bytes_of(&[0.0f32; 4]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("thumbs"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
            label: Some("thumbs"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("thumbs"),
            layout: &globals_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let page_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("thumb page"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/thumbs.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("thumbs"),
            bind_group_layouts: &[Some(&globals_layout), Some(&page_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("thumbs"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Instance>() as u64,
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let capacity = 256;
        Self {
            pipeline,
            globals,
            globals_group,
            page_layout,
            instances: instance_buffer(device, capacity),
            capacity,
            batches: Vec::new(),
            max_dim,
            page_side: PAGE.min(max_dim),
            slot: 0,
            pages: Vec::new(),
            slots: HashMap::new(),
            free: Vec::new(),
            frame: 0,
        }
    }

    /// Start over for thumbnails of `cell` pixels (a new grid size or display scale).
    pub fn reset(&mut self, cell: u32) {
        let slot = (cell + 2).min(self.max_dim);
        self.slot = slot;
        self.page_side = PAGE.max(slot.next_power_of_two()).min(self.max_dim);
        self.pages.clear();
        self.slots.clear();
        self.free.clear();
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.slots.contains_key(path)
    }

    fn per_row(&self) -> u32 {
        (self.page_side / self.slot.max(1)).max(1)
    }

    fn add_page(&mut self, device: &wgpu::Device) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("thumb page"),
            size: wgpu::Extent3d {
                width: self.page_side,
                height: self.page_side,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("thumb page"),
            layout: &self.page_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        let page = self.pages.len();
        self.pages.push(Page {
            texture,
            bind_group,
        });
        let n = self.per_row() * self.per_row();
        self.free.extend((0..n).rev().map(|i| (page, i)));
        log::debug!(
            "thumbnail atlas: page {} ({n} slots of {} px)",
            page + 1,
            self.slot
        );
    }

    /// Store `thumb` for `path`. When full, the least recently drawn thumbnail that isn't
    /// `wanted` makes room; if every one is wanted, the atlas grows. Returns evicted paths.
    pub fn insert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: &Path,
        thumb: &Thumb,
        wanted: impl Fn(&Path) -> bool,
    ) -> Vec<PathBuf> {
        let mut evicted = Vec::new();
        let (page, index) = if let Some(s) = self.slots.remove(path) {
            (s.page, s.index)
        } else if let Some(free) = self.free.pop() {
            free
        } else {
            let victim = self
                .slots
                .iter()
                .filter(|(p, _)| !wanted(p))
                .min_by_key(|(_, s)| s.used)
                .map(|(p, _)| p.clone());
            match victim.and_then(|v| self.slots.remove(&v).map(|s| (v, s))) {
                Some((v, s)) => {
                    evicted.push(v);
                    (s.page, s.index)
                }
                None => {
                    self.add_page(device);
                    match self.free.pop() {
                        Some(f) => f,
                        None => return evicted,
                    }
                }
            }
        };
        // Clamp to the slot, keeping a 1 px clear border so filtering never picks up a
        // neighbour or a previous occupant.
        let inner = self.slot.saturating_sub(2).max(1);
        let w = thumb.size[0].min(inner);
        let h = thumb.size[1].min(inner);
        let (pw, ph) = (w + 2, h + 2);
        let mut padded = vec![0u8; (pw * ph * 4) as usize];
        let src_stride = thumb.size[0] as usize * 4;
        for y in 0..h as usize {
            let src = &thumb.pixels[y * src_stride..][..w as usize * 4];
            let dst = ((y + 1) * pw as usize + 1) * 4;
            padded[dst..dst + src.len()].copy_from_slice(src);
        }
        let per_row = self.per_row();
        let origin = [(index % per_row) * self.slot, (index / per_row) * self.slot];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.pages[page].texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: origin[0],
                    y: origin[1],
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pw * 4),
                rows_per_image: Some(ph),
            },
            wgpu::Extent3d {
                width: pw,
                height: ph,
                depth_or_array_layers: 1,
            },
        );
        self.slots.insert(
            path.to_path_buf(),
            Slot {
                page,
                index,
                size: [w, h],
                used: self.frame,
            },
        );
        evicted
    }

    /// Build this frame's instances. Thumbnails not in the atlas are skipped.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        draws: &[ThumbDraw],
        viewport: [f32; 2],
    ) {
        self.frame += 1;
        self.batches.clear();
        if draws.is_empty() {
            return;
        }
        queue.write_buffer(
            &self.globals,
            0,
            bytemuck::bytes_of(&[viewport[0], viewport[1], 0.0, 0.0]),
        );
        let side = self.page_side as f32;
        let per_row = self.per_row();
        let mut list: Vec<(usize, Instance)> = Vec::with_capacity(draws.len());
        for d in draws {
            let Some(slot) = self.slots.get_mut(d.path) else {
                continue;
            };
            slot.used = self.frame;
            let x = (slot.index % per_row) * self.slot + 1;
            let y = (slot.index / per_row) * self.slot + 1;
            list.push((
                slot.page,
                Instance {
                    rect: d.rect,
                    uv: [
                        x as f32 / side,
                        y as f32 / side,
                        slot.size[0] as f32 / side,
                        slot.size[1] as f32 / side,
                    ],
                    params: [d.alpha, 0.0, 0.0, 0.0],
                },
            ));
        }
        list.sort_by_key(|(p, _)| *p);
        if list.len() > self.capacity {
            self.capacity = list.len().next_power_of_two();
            self.instances = instance_buffer(device, self.capacity);
        }
        let instances: Vec<Instance> = list.iter().map(|(_, i)| *i).collect();
        if !instances.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        }
        let mut start = 0;
        for (i, (page, _)) in list.iter().enumerate() {
            let end = i + 1;
            if end == list.len() || list[end].0 != *page {
                self.batches.push((*page, start as u32..end as u32));
                start = end;
            }
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.batches.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for (page, range) in &self.batches {
            pass.set_bind_group(1, &self.pages[*page].bind_group, &[]);
            pass.draw(0..6, range.clone());
        }
    }
}
