//! wgpu device and surface setup, and per-frame orchestration.

pub mod image;
pub mod prims;
pub mod text;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use winit::window::Window;

use crate::config::theme::Rgba;
use crate::decode::DecodedImage;
use crate::state::view::Affine;
use image::{AlphaStyle, GpuImage, ImagePipeline};
use prims::{Prim, PrimPipeline};
use text::{TextItem, TextSystem};

pub struct ImageDraw<'a> {
    pub path: &'a Path,
    pub affine: Affine,
    pub alpha: f32,
    pub nearest: bool,
}

/// Everything one frame shows, in physical pixels. Built by `ui`, drawn here.
pub struct Scene<'a> {
    pub clear: Rgba,
    /// Images are clipped to this rect (x, y, w, h): the area above the status bar.
    pub image_clip: [f32; 4],
    pub alpha: AlphaStyle,
    pub images: Vec<ImageDraw<'a>>,
    pub prims: Vec<Prim>,
    pub texts: Vec<TextItem>,
}

pub struct Renderer {
    window: Arc<Window>,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    view_format: wgpu::TextureFormat,
    images: ImagePipeline,
    prims: PrimPipeline,
    text: TextSystem,
    textures: HashMap<PathBuf, GpuImage>,
    max_dim: u32,
}

impl Renderer {
    pub fn new(
        window: Arc<Window>,
        display: winit::event_loop::OwnedDisplayHandle,
    ) -> anyhow::Result<Self> {
        let mut desc =
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display));
        if wgpu::Backends::from_env().is_none() {
            desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        }
        let instance = wgpu::Instance::new(desc);
        let surface = instance
            .create_surface(window.clone())
            .context("create surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            // The integrated GPU is plenty for images and doesn't wake a sleeping dGPU.
            power_preference:
                wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower),
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no suitable GPU adapter")?;
        let info = adapter.get_info();
        log::debug!("adapter: {} ({:?})", info.name, info.backend);
        let limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
        let max_dim = limits.max_texture_dimension_2d;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("remage"),
            required_limits: limits,
            ..Default::default()
        }))
        .context("request GPU device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb() || f.add_srgb_suffix() != *f)
            .or_else(|| caps.formats.first().copied())
            .context("surface supports no formats")?;
        // Render through an sRGB view so blending happens in linear light.
        let view_format = format.add_srgb_suffix();
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: if view_format == format {
                vec![]
            } else {
                vec![view_format]
            },
            desired_maximum_frame_latency: 1,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        Ok(Self {
            images: ImagePipeline::new(&device, view_format),
            prims: PrimPipeline::new(&device, view_format),
            text: TextSystem::new(&device, &queue, view_format),
            window,
            instance,
            surface,
            device,
            queue,
            config,
            view_format,
            textures: HashMap::new(),
            max_dim,
        })
    }

    pub fn max_texture_dim(&self) -> u32 {
        self.max_dim
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn measure(&mut self, item: &TextItem) -> f32 {
        self.text.measure(item)
    }

    pub fn set_fonts(&mut self, db: glyphon::fontdb::Database) {
        self.text.set_fonts(&self.device, &self.queue, db);
    }

    pub fn is_ready(&self, path: &Path) -> bool {
        self.textures.get(path).is_some_and(GpuImage::is_ready)
    }

    /// Upload up to `budget` bytes of `img`. Returns true once it is ready to draw.
    pub fn upload(&mut self, path: &Path, img: &DecodedImage, budget: usize) -> bool {
        let gpu = match self.textures.get_mut(path) {
            Some(g) => g,
            None => {
                let g = self.images.create(&self.device, img);
                self.textures.entry(path.to_path_buf()).or_insert(g)
            }
        };
        self.images
            .upload(&self.device, &self.queue, gpu, img, budget)
    }

    /// Drop textures that are no longer wanted.
    pub fn retain(&mut self, keep: impl Fn(&Path) -> bool) {
        self.textures.retain(|p, _| keep(p));
    }

    pub fn render(&mut self, scene: &Scene) -> anyhow::Result<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                self.surface.configure(&self.device, &self.config);
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self.instance.create_surface(self.window.clone())?;
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => anyhow::bail!("surface validation error"),
        };
        let size = [self.config.width, self.config.height];
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        let commands = self.encode(scene, &view, size)?;
        self.queue.submit([commands]);
        self.window.pre_present_notify();
        self.queue.present(frame);
        self.text.trim();
        Ok(())
    }

    /// Record the frame into `view`. Uniform and text uploads happen here too.
    fn encode(
        &mut self,
        scene: &Scene,
        view: &wgpu::TextureView,
        size: [u32; 2],
    ) -> anyhow::Result<wgpu::CommandBuffer> {
        let viewport = [size[0] as f32, size[1] as f32];

        let draws: Vec<_> = scene
            .images
            .iter()
            .filter_map(|d| {
                self.textures
                    .get(d.path)
                    .filter(|g| g.is_ready())
                    .map(|g| (d, g))
            })
            .collect();
        for (d, gpu) in &draws {
            self.images
                .prepare(&self.queue, gpu, d.affine, viewport, d.alpha, &scene.alpha);
        }
        self.prims
            .prepare(&self.device, &self.queue, &scene.prims, viewport);
        self.text
            .prepare(&self.device, &self.queue, &scene.texts, size)?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let [r, g, b, a] = scene.clear.linear();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: r as f64,
                            g: g as f64,
                            b: b as f64,
                            a: a as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some((x, y, w, h)) = clip(scene.image_clip, size) {
                pass.set_scissor_rect(x, y, w, h);
                for (d, gpu) in &draws {
                    self.images.draw(&mut pass, gpu, d.nearest);
                }
                pass.set_scissor_rect(0, 0, size[0], size[1]);
            }
            self.prims.draw(&mut pass);
            self.text.render(&mut pass)?;
        }
        Ok(encoder.finish())
    }

    /// Debug aid (`REMAGE_SNAPSHOT=out.png`): render `scene` offscreen and save it.
    pub fn snapshot(&mut self, scene: &Scene, path: &Path) -> anyhow::Result<()> {
        let size = [self.config.width, self.config.height];
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("snapshot"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.view_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let commands = self.encode(scene, &view, size)?;
        let row = (size[0] * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("snapshot"),
            size: row as u64 * size[1] as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(size[1]),
                },
            },
            texture.size(),
        );
        self.queue.submit([commands, encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        let data = buffer.get_mapped_range(..)?;
        let bgra = matches!(self.view_format, wgpu::TextureFormat::Bgra8UnormSrgb);
        let mut pixels = Vec::with_capacity((size[0] * size[1] * 4) as usize);
        for y in 0..size[1] as usize {
            let line = &data[y * row as usize..][..size[0] as usize * 4];
            for px in line.chunks_exact(4) {
                let rgb = if bgra {
                    [px[2], px[1], px[0]]
                } else {
                    [px[0], px[1], px[2]]
                };
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        ::image::RgbaImage::from_raw(size[0], size[1], pixels)
            .context("snapshot buffer size")?
            .save(path)?;
        Ok(())
    }
}

/// Integer scissor rect inside the surface, or `None` if empty.
fn clip(r: [f32; 4], size: [u32; 2]) -> Option<(u32, u32, u32, u32)> {
    let x0 = r[0].max(0.0).round() as u32;
    let y0 = r[1].max(0.0).round() as u32;
    let x1 = ((r[0] + r[2]).round() as u32).min(size[0]);
    let y1 = ((r[1] + r[3]).round() as u32).min(size[1]);
    (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
}
