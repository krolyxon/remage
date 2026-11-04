//! Image textures: incremental row uploads, GPU mipmaps, linear/nearest sampling, and tiles
//! for images larger than the GPU's texture limit.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::decode::DecodedImage;
use crate::state::view::Affine;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    m: [f32; 4],
    t: [f32; 2],
    viewport: [f32; 2],
    bg: [f32; 4],
    dot: [f32; 4],
    params: [f32; 4],
    /// Gamma, brightness, contrast, and 1 when any differs from neutral.
    adjust: [f32; 4],
}

/// How images are painted: the area behind transparent pixels, and display adjustments.
#[derive(Clone, Copy, Debug)]
pub struct AlphaStyle {
    /// Gamma, brightness, contrast.
    pub adjust: [f32; 3],
    /// 0 solid, 1 dots, 2 checker.
    pub mode: u32,
    pub bg: [f32; 4],
    pub dot: [f32; 4],
    pub cell: f32,
    pub radius: f32,
}

/// One texture of an image: all of it, or a tile of a huge one.
struct Tile {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    origin: [u32; 2],
    size: [u32; 2],
}

pub struct GpuImage {
    tiles: Vec<Tile>,
    size: [u32; 2],
    /// The frame these textures hold (or are being filled with).
    frame: usize,
    /// Upload progress: the tile being filled and its rows done.
    tile: usize,
    rows_done: u32,
    ready: bool,
}

impl GpuImage {
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn frame(&self) -> usize {
        self.frame
    }

    pub fn size(&self) -> [u32; 2] {
        self.size
    }
}

/// Split `size` into tiles no larger than `max` on either side.
pub fn tiles(size: [u32; 2], max: u32) -> Vec<([u32; 2], [u32; 2])> {
    let max = max.max(1);
    let mut out = Vec::new();
    for y in (0..size[1].max(1)).step_by(max as usize) {
        for x in (0..size[0].max(1)).step_by(max as usize) {
            out.push(([x, y], [(size[0] - x).min(max), (size[1] - y).min(max)]));
        }
    }
    out
}

pub struct ImagePipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    linear: wgpu::BindGroup,
    nearest: wgpu::BindGroup,
    mip_pipeline: wgpu::RenderPipeline,
    mip_layout: wgpu::BindGroupLayout,
    mip_sampler: wgpu::Sampler,
}

fn sampler_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    filter: wgpu::FilterMode,
) -> wgpu::BindGroup {
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("image sampler"),
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("image sampler"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Sampler(&sampler),
        }],
    })
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
            ],
        });
        let sampler_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image sampler"),
            entries: &[sampler_entry(0)],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/image.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image"),
            bind_group_layouts: &[Some(&layout), Some(&sampler_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
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

        let mip_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mip"),
            entries: &[texture_entry(0), sampler_entry(1)],
        });
        let blit = device.create_shader_module(wgpu::include_wgsl!("shaders/blit.wgsl"));
        let mip_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mip"),
            bind_group_layouts: &[Some(&mip_layout)],
            immediate_size: 0,
        });
        let mip_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mip"),
            layout: Some(&mip_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let mip_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mip"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            pipeline,
            linear: sampler_group(device, &sampler_layout, wgpu::FilterMode::Linear),
            nearest: sampler_group(device, &sampler_layout, wgpu::FilterMode::Nearest),
            layout,
            mip_pipeline,
            mip_layout,
            mip_sampler,
        }
    }

    /// Allocate textures for `img`, tiled at `max_dim`; fill them with [`ImagePipeline::upload`].
    pub fn create(&self, device: &wgpu::Device, img: &DecodedImage, max_dim: u32) -> GpuImage {
        let tiles = tiles(img.size, max_dim)
            .into_iter()
            .map(|(origin, size)| self.tile(device, origin, size))
            .collect::<Vec<_>>();
        if tiles.len() > 1 {
            log::debug!(
                "{}×{} image in {} tiles",
                img.size[0],
                img.size[1],
                tiles.len()
            );
        }
        GpuImage {
            tiles,
            size: img.size,
            frame: 0,
            tile: 0,
            rows_done: 0,
            ready: false,
        }
    }

    fn tile(&self, device: &wgpu::Device, origin: [u32; 2], [w, h]: [u32; 2]) -> Tile {
        let mips = 32 - w.max(h).max(1).leading_zeros();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("image uniforms"),
            contents: bytemuck::bytes_of(&Uniforms::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        Tile {
            texture,
            bind_group,
            uniforms,
            origin,
            size: [w, h],
        }
    }

    /// Start filling the textures with another frame; finish with [`ImagePipeline::upload`].
    pub fn restart(&self, gpu: &mut GpuImage, frame: usize) {
        gpu.frame = frame;
        gpu.tile = 0;
        gpu.rows_done = 0;
        gpu.ready = false;
    }

    /// Upload up to `budget` bytes of rows. Returns true once the image is complete.
    pub fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gpu: &mut GpuImage,
        img: &DecodedImage,
        budget: usize,
    ) -> bool {
        let pixels = img.frame(gpu.frame);
        let stride = img.size[0] as usize * 4;
        let mut left = budget;
        while !gpu.ready {
            let tile = &gpu.tiles[gpu.tile];
            let [w, h] = tile.size;
            let row_bytes = w as usize * 4;
            let rows = ((left / row_bytes.max(1)) as u32).clamp(1, h - gpu.rows_done);
            let y = tile.origin[1] + gpu.rows_done;
            let start = y as usize * stride + tile.origin[0] as usize * 4;
            let end = start + (rows as usize - 1) * stride + row_bytes;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tile.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: gpu.rows_done,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels[start..end],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width: w,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
            gpu.rows_done += rows;
            left = left.saturating_sub(rows as usize * row_bytes);
            if gpu.rows_done == h {
                self.generate_mips(device, queue, &gpu.tiles[gpu.tile].texture);
                gpu.tile += 1;
                gpu.rows_done = 0;
                gpu.ready = gpu.tile == gpu.tiles.len();
            }
            if left == 0 {
                break;
            }
        }
        if !gpu.ready {
            queue.submit([]);
        }
        gpu.ready
    }

    fn generate_mips(&self, device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mips"),
        });
        let level_view = |level| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        for level in 1..texture.mip_level_count() {
            let src = level_view(level - 1);
            let dst = level_view(level);
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mip"),
                layout: &self.mip_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.mip_sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mip"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }

    /// Write this frame's placement; call before the render pass begins.
    pub fn prepare(
        &self,
        queue: &wgpu::Queue,
        gpu: &GpuImage,
        affine: Affine,
        viewport: [f32; 2],
        alpha: f32,
        style: &AlphaStyle,
    ) {
        let adjust = style.adjust;
        let on = adjust != [1.0, 0.0, 1.0];
        let [iw, ih] = [gpu.size[0] as f32, gpu.size[1] as f32];
        let m = affine.m;
        for tile in &gpu.tiles {
            // The tile's corner and extent in the image's 0..1 coordinates.
            let (u0, v0) = (tile.origin[0] as f32 / iw, tile.origin[1] as f32 / ih);
            let (du, dv) = (tile.size[0] as f32 / iw, tile.size[1] as f32 / ih);
            let u = Uniforms {
                m: [m[0] * du, m[1] * dv, m[2] * du, m[3] * dv],
                t: [
                    affine.t[0] + m[0] * u0 + m[1] * v0,
                    affine.t[1] + m[2] * u0 + m[3] * v0,
                ],
                viewport,
                bg: style.bg,
                dot: style.dot,
                params: [alpha, style.mode as f32, style.cell, style.radius],
                adjust: [adjust[0], adjust[1], adjust[2], on as u32 as f32],
            };
            queue.write_buffer(&tile.uniforms, 0, bytemuck::bytes_of(&u));
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, gpu: &GpuImage, nearest: bool) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(1, if nearest { &self.nearest } else { &self.linear }, &[]);
        for tile in &gpu.tiles {
            pass.set_bind_group(0, &tile.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiling_covers_the_image() {
        assert_eq!(tiles([10, 5], 16), [([0, 0], [10, 5])]);
        let t = tiles([10, 5], 4);
        assert_eq!(t.len(), 6);
        assert_eq!(t[2], ([8, 0], [2, 4]));
        assert_eq!(t[5], ([8, 4], [2, 1]));
        let area: u32 = t.iter().map(|(_, s)| s[0] * s[1]).sum();
        assert_eq!(area, 50);
    }
}
