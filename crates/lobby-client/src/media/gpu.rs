//! Screen Share pictures prepared on the GPU: the D3D11 video processor scales and converts a
//! captured BGRA texture into an NV12 texture leased from a pool, on the capture's own device, so
//! the capture thread reads nothing back. The hardware encoder ([`super::mf`]) takes the textures
//! as they are.
//!
//! A pool texture is held through [`Lease`]s: by the slot's pending picture, the encoder's fresh
//! and last ones and the hardware encoder's input samples. It goes back
//! to its pool when the last lease drops. The GPU is done with it by then: every command on a
//! texture goes through its device's one immediate context, which runs them in order, and the
//! hardware encoder holds a sample for as long as it needs the texture in it.
#![allow(unsafe_code)]

use std::{
    mem::ManuallyDrop,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use lobby_protocol::encoder::fit;
use windows::{
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_11_1,
            },
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
                D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
                D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
                D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device,
                ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, ID3D11VideoContext1,
                ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
                ID3D11VideoProcessorOutputView,
            },
            Dxgi::{
                Common::{
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
                },
                IDXGIAdapter,
            },
        },
    },
    core::Interface,
};

use super::{MediaError, screen::Rect, video::I420};

/// NV12 textures per pool. The slot's picture, the encoder's fresh and last ones and the one
/// being drawn hold four at most; the rest are for earlier pictures the hardware encoder holds.
pub(crate) const POOL_TEXTURES: usize = 6;

fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> MediaError {
    move |e| MediaError::Capture(format!("{what}: {e}"))
}

/// A Direct3D 11 device on `adapter`, or on the default one.
pub(crate) fn create_device(adapter: Option<&IDXGIAdapter>) -> Result<ID3D11Device, MediaError> {
    let driver = if adapter.is_some() {
        D3D_DRIVER_TYPE_UNKNOWN
    } else {
        D3D_DRIVER_TYPE_HARDWARE
    };
    let mut device = None;
    // SAFETY: the out pointer is a live `Option`, and the feature levels outlive the call.
    unsafe {
        D3D11CreateDevice(
            adapter,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .map_err(failed("Direct3D device"))?;
    device.ok_or_else(|| MediaError::Capture("Direct3D made no device".into()))
}

fn describe(texture: &ID3D11Texture2D) -> D3D11_TEXTURE2D_DESC {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: the out pointer is a live description.
    unsafe { texture.GetDesc(&mut desc) };
    desc
}

pub(crate) fn texture_size(texture: &ID3D11Texture2D) -> (u32, u32) {
    let desc = describe(texture);
    (desc.Width, desc.Height)
}

fn create_texture(
    device: &ID3D11Device,
    desc: &D3D11_TEXTURE2D_DESC,
) -> Result<ID3D11Texture2D, MediaError> {
    let mut texture = None;
    // SAFETY: the description and the out pointer are live; there is no initial data.
    unsafe { device.CreateTexture2D(desc, None, Some(&mut texture)) }.map_err(failed("texture"))?;
    texture.ok_or_else(|| MediaError::Capture("Direct3D made no texture".into()))
}

fn texture_desc(size: (u32, u32), format: DXGI_FORMAT) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    }
}

fn enumerator(
    video: &ID3D11VideoDevice,
    input: (u32, u32),
    output: (u32, u32),
) -> Result<ID3D11VideoProcessorEnumerator, MediaError> {
    let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        },
        InputWidth: input.0,
        InputHeight: input.1,
        OutputFrameRate: DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        },
        OutputWidth: output.0,
        OutputHeight: output.1,
        Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
    };
    // SAFETY: the description is live for the call.
    unsafe { video.CreateVideoProcessorEnumerator(&content) }.map_err(failed("video processor"))
}

/// NV12 textures of one size on one device, handed out through [`Lease`]s.
pub(crate) struct Pool {
    size: (u32, u32),
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    textures: Vec<ID3D11Texture2D>,
    free: Mutex<Vec<usize>>,
}

impl Pool {
    fn free(&self) -> MutexGuard<'_, Vec<usize>> {
        // Pushing or popping an index never leaves the list half changed.
        self.free.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lease(self: &Arc<Pool>) -> Option<Lease> {
        let index = self.free().pop()?;
        Some(Lease(Arc::new(Held {
            pool: self.clone(),
            index,
        })))
    }
}

/// A hold on one pool texture. Clones share it; the texture goes back to its pool when the last
/// one drops, and the pool, with its textures, goes when its last lease does.
#[derive(Clone)]
pub(crate) struct Lease(Arc<Held>);

struct Held {
    pool: Arc<Pool>,
    index: usize,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.pool.free().push(self.index);
    }
}

impl Lease {
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.0.pool.textures[self.0.index]
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.0.pool.device
    }

    pub fn size(&self) -> (u32, u32) {
        self.0.pool.size
    }
}

/// A picture the GPU prepared: NV12 in a leased texture.
pub(crate) struct GpuPicture {
    pub lease: Lease,
    pub captured: Instant,
}

/// The video processor for one input size and format and one output size, and the pool it draws
/// into.
struct Pipeline {
    input: (u32, u32),
    format: DXGI_FORMAT,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    pool: Arc<Pool>,
    views: Vec<ID3D11VideoProcessorOutputView>,
}

/// Scales and converts BGRA textures into pool NV12 textures (BT.709, limited range, as the CPU
/// converter does), or scales NV12 ones, on the device the textures come from.
pub(crate) struct Blitter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    lock: ID3D11Multithread,
    textures: usize,
    pipeline: Option<Pipeline>,
}

impl Blitter {
    /// Pools of `textures` textures. Fails where the device has no video processor that writes
    /// NV12, as on WARP.
    pub fn new(device: &ID3D11Device, textures: usize) -> Result<Blitter, MediaError> {
        // SAFETY: plain COM calls on a live device.
        let blitter = unsafe {
            let context = device
                .GetImmediateContext()
                .map_err(failed("Direct3D context"))?;
            let lock: ID3D11Multithread = context.cast().map_err(failed("Direct3D lock"))?;
            // The encoder's thread reads textures back through this context too; windows-capture
            // never turns this on.
            let _ = lock.SetMultithreadProtected(true);
            Blitter {
                video: device.cast().map_err(failed("video device"))?,
                video_context: context.cast().map_err(failed("video context"))?,
                device: device.clone(),
                context,
                lock,
                textures,
                pipeline: None,
            }
        };
        let enumerator = enumerator(&blitter.video, (1920, 1080), (1280, 720))?;
        // SAFETY: a plain query on a live enumerator.
        let support = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
            .map_err(failed("NV12 output"))?;
        if support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
            return Err(MediaError::Capture(
                "the video processor cannot write NV12".into(),
            ));
        }
        Ok(blitter)
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    fn pipeline(
        &self,
        input: (u32, u32),
        format: DXGI_FORMAT,
        output: (u32, u32),
    ) -> Result<Pipeline, MediaError> {
        let enumerator = enumerator(&self.video, input, output)?;
        let input_space = if format == DXGI_FORMAT_NV12 {
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
        } else {
            DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709
        };
        // SAFETY: COM calls on live objects; the view descriptions and out pointers are live.
        unsafe {
            let processor = self
                .video
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(failed("video processor"))?;
            self.lock.Enter();
            self.video_context
                .VideoProcessorSetStreamColorSpace1(&processor, 0, input_space);
            self.video_context.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            self.video_context
                .VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            self.lock.Leave();
            let desc = texture_desc(output, DXGI_FORMAT_NV12);
            let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut textures = Vec::with_capacity(self.textures);
            let mut views = Vec::with_capacity(self.textures);
            for _ in 0..self.textures {
                let texture = create_texture(&self.device, &desc)?;
                let mut view = None;
                self.video
                    .CreateVideoProcessorOutputView(
                        &texture,
                        &enumerator,
                        &view_desc,
                        Some(&mut view),
                    )
                    .map_err(failed("NV12 view"))?;
                views.push(view.ok_or_else(|| MediaError::Capture("no NV12 view".into()))?);
                textures.push(texture);
            }
            Ok(Pipeline {
                input,
                format,
                enumerator,
                processor,
                pool: Arc::new(Pool {
                    size: output,
                    context: self.context.clone(),
                    device: self.device.clone(),
                    free: Mutex::new((0..textures.len()).collect()),
                    textures,
                }),
                views,
            })
        }
    }

    /// Draws `texture` into a pool texture, scaled down to fit `max` as the CPU converter does.
    /// `None` when every texture of the pool is held: the picture is dropped, never waited for.
    pub fn blit(
        &mut self,
        texture: &ID3D11Texture2D,
        max: (u32, u32),
    ) -> Result<Option<Lease>, MediaError> {
        let desc = describe(texture);
        let (input, format) = ((desc.Width, desc.Height), desc.Format);
        let output = fit(input, max);
        if self
            .pipeline
            .as_ref()
            .is_none_or(|p| p.input != input || p.format != format || p.pool.size != output)
        {
            self.pipeline = Some(self.pipeline(input, format, output)?);
        }
        let p = self.pipeline.as_ref().expect("built above");
        let Some(lease) = p.pool.lease() else {
            return Ok(None);
        };
        let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view = None;
        // SAFETY: COM calls on live objects. The stream borrows the input view, which is dropped
        // after the draw, once.
        unsafe {
            self.video
                .CreateVideoProcessorInputView(
                    texture,
                    &p.enumerator,
                    &view_desc,
                    Some(&mut input_view),
                )
                .map_err(failed("BGRA view"))?;
            let streams = [D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(input_view),
                ..Default::default()
            }];
            self.lock.Enter();
            let drawn = self.video_context.VideoProcessorBlt(
                &p.processor,
                &p.views[lease.0.index],
                0,
                &streams,
            );
            self.lock.Leave();
            let [stream] = streams;
            drop(ManuallyDrop::into_inner(stream.pInputSurface));
            drawn.map_err(failed("video processor draw"))?;
        }
        Ok(Some(lease))
    }
}

/// An NV12 texture the GPU copies a picture into and the video processor reads.
pub(crate) fn nv12_texture(
    device: &ID3D11Device,
    size: (u32, u32),
) -> Result<ID3D11Texture2D, MediaError> {
    create_texture(device, &texture_desc(size, DXGI_FORMAT_NV12))
}

/// A texture of `format` the CPU reads.
pub(crate) fn staging(
    device: &ID3D11Device,
    size: (u32, u32),
    format: DXGI_FORMAT,
) -> Result<ID3D11Texture2D, MediaError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        ..texture_desc(size, format)
    };
    create_texture(device, &desc)
}

/// The video processor for one decoded picture size and one size out, and the BGRA texture it
/// draws into.
struct Scaling {
    input: (u32, u32),
    shown: (u32, u32),
    output: (u32, u32),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    target: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
}

/// Scales a decoded NV12 picture down and converts it to BGRA (BT.709, limited range in, full
/// range out, as the CPU conversion does) in one draw of the video processor, then copies it into
/// a staging texture: only the pixels the view paints are read back.
pub(crate) struct Scaler {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    lock: ID3D11Multithread,
    scaling: Option<Scaling>,
}

impl Scaler {
    /// Fails where the device's video processor cannot write BGRA.
    pub fn new(device: &ID3D11Device) -> Result<Scaler, MediaError> {
        // SAFETY: plain COM calls on a live device.
        let scaler = unsafe {
            let context = device
                .GetImmediateContext()
                .map_err(failed("Direct3D context"))?;
            Scaler {
                video: device.cast().map_err(failed("video device"))?,
                video_context: context.cast().map_err(failed("video context"))?,
                lock: context.cast().map_err(failed("Direct3D lock"))?,
                device: device.clone(),
                context,
                scaling: None,
            }
        };
        let enumerator = enumerator(&scaler.video, (1920, 1080), (1280, 720))?;
        // SAFETY: a plain query on a live enumerator.
        let support = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM) }
            .map_err(failed("BGRA output"))?;
        if support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
            return Err(MediaError::Video(
                "the video processor cannot write BGRA".into(),
            ));
        }
        Ok(scaler)
    }

    fn scaling(
        &self,
        input: (u32, u32),
        shown: (u32, u32),
        output: (u32, u32),
    ) -> Result<Scaling, MediaError> {
        let enumerator = enumerator(&self.video, input, output)?;
        let desc = texture_desc(output, DXGI_FORMAT_B8G8R8A8_UNORM);
        let target = create_texture(&self.device, &desc)?;
        let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let rect = |(w, h): (u32, u32)| RECT {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        // SAFETY: COM calls on live objects; the descriptions, rects and out pointers are live.
        unsafe {
            let processor = self
                .video
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(failed("video processor"))?;
            let mut view = None;
            self.video
                .CreateVideoProcessorOutputView(&target, &enumerator, &view_desc, Some(&mut view))
                .map_err(failed("BGRA view"))?;
            let view = view.ok_or_else(|| MediaError::Video("no BGRA view".into()))?;
            let vc = &self.video_context;
            self.lock.Enter();
            vc.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            vc.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            // The decoded picture is whole macroblocks: the rows below what is shown are not
            // part of it.
            vc.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&rect(shown)));
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&rect(output)));
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&rect(output)));
            self.lock.Leave();
            Ok(Scaling {
                input,
                shown,
                output,
                enumerator,
                processor,
                target,
                view,
            })
        }
    }

    pub fn draw(
        &mut self,
        texture: &ID3D11Texture2D,
        shown: (u32, u32),
        staging: &ID3D11Texture2D,
    ) -> Result<(), MediaError> {
        let input = texture_size(texture);
        let output = texture_size(staging);
        if self
            .scaling
            .as_ref()
            .is_none_or(|s| (s.input, s.shown, s.output) != (input, shown, output))
        {
            self.scaling = Some(self.scaling(input, shown, output)?);
        }
        let s = self.scaling.as_ref().expect("built above");
        let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view = None;
        // SAFETY: COM calls on live objects. The stream borrows the input view, which is dropped
        // after the draw, once. The copy runs after the draw on the one immediate context.
        unsafe {
            self.video
                .CreateVideoProcessorInputView(
                    texture,
                    &s.enumerator,
                    &view_desc,
                    Some(&mut input_view),
                )
                .map_err(failed("NV12 view"))?;
            let streams = [D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(input_view),
                ..Default::default()
            }];
            self.lock.Enter();
            let drawn = self
                .video_context
                .VideoProcessorBlt(&s.processor, &s.view, 0, &streams);
            self.lock.Leave();
            let [stream] = streams;
            drop(ManuallyDrop::into_inner(stream.pInputSurface));
            drawn.map_err(failed("video processor draw"))?;
            self.context.CopyResource(staging, &s.target);
        }
        Ok(())
    }
}

/// Calls `read` with a BGRA staging texture's bytes and their row pitch, once the GPU is done
/// writing them.
pub(crate) fn read_bgra<T>(
    context: &ID3D11DeviceContext,
    staging: &ID3D11Texture2D,
    read: impl FnOnce(&[u8], usize) -> T,
) -> Result<T, MediaError> {
    let h = texture_size(staging).1 as usize;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: COM calls on live objects, on a context made thread safe. While mapped, the
    // texture holds `h` rows of `RowPitch` bytes; it is unmapped once `read` is done with them.
    unsafe {
        context
            .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(failed("BGRA readback"))?;
        let pitch = mapped.RowPitch as usize;
        let bgra = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h);
        let out = read(bgra, pitch);
        context.Unmap(staging, 0);
        Ok(out)
    }
}

/// Calls `read` with an NV12 staging texture's bytes and their row pitch, once the GPU is done
/// writing them: `height` rows of luma, then `height / 2` of interleaved chroma.
pub(crate) fn read_nv12<T>(
    context: &ID3D11DeviceContext,
    staging: &ID3D11Texture2D,
    read: impl FnOnce(&[u8], usize) -> T,
) -> Result<T, MediaError> {
    let h = texture_size(staging).1 as usize;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: COM calls on live objects, on a context made thread safe. While mapped, the
    // texture holds `RowPitch`-byte rows, `h` of luma and `h / 2` of chroma; it is unmapped once
    // `read` is done with them.
    unsafe {
        context
            .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(failed("NV12 readback"))?;
        let pitch = mapped.RowPitch as usize;
        let nv12 = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (h + h / 2));
        let out = read(nv12, pitch);
        context.Unmap(staging, 0);
        Ok(out)
    }
}

#[derive(Default)]
pub(crate) struct Readback {
    staging: Option<ID3D11Texture2D>,
}

impl Readback {
    pub fn read(&mut self, lease: &Lease, out: &mut I420) -> Result<(), MediaError> {
        let pool = &lease.0.pool;
        let fits = self
            .staging
            .as_ref()
            .is_some_and(|s| texture_size(s) == pool.size);
        if !fits {
            self.staging = Some(staging(&pool.device, pool.size, DXGI_FORMAT_NV12)?);
        }
        let staging = self.staging.as_ref().expect("made above");
        // SAFETY: a plain copy between two live textures of one size and format.
        unsafe { pool.context.CopyResource(staging, lease.texture()) };
        read_nv12(&pool.context, staging, |nv12, pitch| {
            nv12_to_i420(nv12, pitch, pool.size, out);
        })
    }
}

fn nv12_to_i420(nv12: &[u8], pitch: usize, (w, h): (u32, u32), out: &mut I420) {
    out.resize(w, h);
    let (w, h) = (w as usize, h as usize);
    for (row, y) in out.y.chunks_exact_mut(w).enumerate() {
        y.copy_from_slice(&nv12[row * pitch..][..w]);
    }
    let chroma = &nv12[pitch * h..];
    let cw = w / 2;
    let rows = out.u.chunks_exact_mut(cw).zip(out.v.chunks_exact_mut(cw));
    for (row, (u, v)) in rows.enumerate() {
        let line = &chroma[row * pitch..][..cw * 2];
        for (i, uv) in line.chunks_exact(2).enumerate() {
            u[i] = uv[0];
            v[i] = uv[1];
        }
    }
}

/// A BGRA texture filled from the CPU, on a device of its own: the test pattern's picture on the
/// GPU path, where a capture would hand over its own texture.
pub(crate) struct Upload {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
    size: (u32, u32),
}

impl Upload {
    pub fn new(adapter: Option<&IDXGIAdapter>, size: (u32, u32)) -> Result<Upload, MediaError> {
        let device = create_device(adapter)?;
        let desc = D3D11_TEXTURE2D_DESC {
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            ..texture_desc(size, DXGI_FORMAT_B8G8R8A8_UNORM)
        };
        let texture = create_texture(&device, &desc)?;
        // SAFETY: a plain COM call on a live device.
        let context =
            unsafe { device.GetImmediateContext() }.map_err(failed("Direct3D context"))?;
        Ok(Upload {
            device,
            context,
            texture,
            size,
        })
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// Copies `rect` of `bgra`, a picture of the texture's size with unpadded rows, into it.
    pub fn put(&self, bgra: &[u8], rect: Rect) {
        let (x, y, w, h) = rect;
        let stride = self.size.0 as usize * 4;
        let start = y as usize * stride + x as usize * 4;
        let end = (y + h - 1) as usize * stride + (x + w) as usize * 4;
        assert!(
            w > 0 && h > 0 && x + w <= self.size.0 && y + h <= self.size.1 && end <= bgra.len(),
            "{rect:?} outside the picture"
        );
        let area = D3D11_BOX {
            left: x,
            top: y,
            front: 0,
            right: x + w,
            bottom: y + h,
            back: 1,
        };
        // SAFETY: the box lies inside the texture and its rows inside `bgra` (asserted above).
        unsafe {
            self.context.UpdateSubresource(
                &self.texture,
                0,
                Some(&area),
                bgra[start..end].as_ptr().cast(),
                stride as u32,
                0,
            );
        }
    }
}

/// This machine's adapters, by name, and whether each is software (WARP).
#[cfg(test)]
pub(crate) fn adapters() -> Vec<(String, IDXGIAdapter, bool)> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
    };
    // SAFETY: plain DXGI calls.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.expect("DXGI");
    let mut found = Vec::new();
    // SAFETY: as above.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(found.len() as u32) } {
        // SAFETY: as above.
        let desc = unsafe { adapter.GetDesc1() }.expect("an adapter description");
        let end = desc.Description.iter().position(|c| *c == 0).unwrap_or(128);
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
        let name = String::from_utf16_lossy(&desc.Description[..end]);
        found.push((name, adapter.cast().expect("an adapter"), software));
    }
    found
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::media::video::Converter;

    const SIZE: (u32, u32) = (1920, 1080);
    const WHOLE: Rect = (0, 0, SIZE.0, SIZE.1);

    /// An upload on every adapter whose video processor writes NV12, with its name. The others
    /// are skipped, and the test output says so.
    fn gpus() -> Vec<(String, Upload)> {
        let found: Vec<_> = adapters()
            .into_iter()
            .filter_map(|(name, adapter, _)| {
                let gpu = Upload::new(Some(&adapter), SIZE)
                    .and_then(|upload| Blitter::new(upload.device(), 1).map(|_| upload));
                gpu.inspect_err(|e| eprintln!("skipped {name}: {e}"))
                    .ok()
                    .map(|upload| (name, upload))
            })
            .collect();
        if found.is_empty() {
            eprintln!("no GPU here prepares pictures: nothing was tested");
        }
        found
    }

    /// A device without a video processor, as WARP's, is refused when the GPU path is set up,
    /// not on the share's first picture.
    #[test]
    fn a_device_without_a_video_processor_is_refused_at_start() {
        for (name, adapter, _) in adapters().into_iter().filter(|(.., software)| *software) {
            let device = create_device(Some(&adapter)).unwrap();
            assert!(Blitter::new(&device, 1).is_err(), "{name}");
        }
    }

    const BARS: [[u8; 3]; 8] = [
        [255, 255, 255],
        [0, 255, 255],
        [255, 255, 0],
        [0, 255, 0],
        [255, 0, 255],
        [0, 0, 255],
        [255, 0, 0],
        [0, 0, 0],
    ];

    /// Saturated bars over the top quarter and gradients below, as BGRA: a swapped channel, a
    /// full-range or BT.601 conversion would all show. `seed` shifts the red gradient.
    fn picture(seed: u8) -> Vec<u8> {
        let (w, h) = SIZE;
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for (i, px) in bgra.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let [b, g, r] = if y < h / 4 {
                BARS[(x * 8 / w) as usize]
            } else {
                [
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    seed.wrapping_add(((x + y) * 255 / (w + h)) as u8),
                ]
            };
            px.copy_from_slice(&[b, g, r, 255]);
        }
        bgra
    }

    /// How far apart two planes are: the mean and the 99th percentile of the differences.
    fn apart(a: &[u8], b: &[u8]) -> (f64, u8) {
        assert_eq!(a.len(), b.len());
        let mut d: Vec<u8> = a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)).collect();
        let mean = d.iter().map(|d| f64::from(*d)).sum::<f64>() / d.len() as f64;
        d.sort_unstable();
        (mean, d[d.len() * 99 / 100])
    }

    #[test]
    fn the_gpu_draws_the_picture_the_cpu_converter_does() {
        let bgra = picture(0);
        for (name, upload) in gpus() {
            let mut blitter = Blitter::new(upload.device(), 1).unwrap();
            let mut readback = Readback::default();
            upload.put(&bgra, WHOLE);
            for max in [SIZE, (1280, 720)] {
                let lease = blitter
                    .blit(upload.texture(), max)
                    .unwrap()
                    .expect("a free texture");
                let mut gpu = I420::empty();
                readback.read(&lease, &mut gpu).unwrap();
                let mut cpu = I420::empty();
                Converter::new()
                    .convert(&bgra, SIZE.0, SIZE.1, SIZE.0 * 4, max, &mut cpu)
                    .unwrap();
                assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height), "{name}");
                for (plane, g, c) in [
                    ("Y", &gpu.y, &cpu.y),
                    ("U", &gpu.u, &cpu.u),
                    ("V", &gpu.v, &cpu.v),
                ] {
                    let (mean, p99) = apart(g, c);
                    eprintln!("{name}, at most {max:?}, {plane}: mean {mean:.2}, p99 {p99}");
                    assert!(
                        mean <= 1.0 && p99 <= 3,
                        "{name}, at most {max:?}: {plane} apart by {mean:.2}, p99 {p99}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_held_texture_is_never_drawn_over_until_its_last_holder_lets_go() {
        for (name, upload) in gpus() {
            let mut blitter = Blitter::new(upload.device(), 2).unwrap();
            let mut readback = Readback::default();
            let mut draw = |seed: u8| {
                upload.put(&picture(seed), WHOLE);
                blitter.blit(upload.texture(), SIZE).unwrap()
            };
            let mut read = |lease: &Lease| {
                let mut out = I420::empty();
                readback.read(lease, &mut out).unwrap();
                out.y
            };
            let last = draw(1).expect("a free texture");
            let still = read(&last);
            let reading = last.clone();
            for seed in 2..8 {
                drop(draw(seed).expect("the other texture"));
            }
            assert!(
                read(&reading) == still,
                "{name}: a still screen's picture was drawn over while held"
            );
            let held = last.0.index;
            drop(last);
            let pending = draw(8).expect("the other texture");
            assert!(
                draw(9).is_none(),
                "{name}: both textures held, so the picture is dropped"
            );
            drop(reading);
            let again = draw(10).expect("a texture once its last holder let go");
            assert_eq!(again.0.index, held, "{name}");
            drop((pending, again));
        }
    }

    #[test]
    fn a_new_size_gets_a_new_pool_and_the_old_one_goes_with_its_last_lease() {
        for (name, upload) in gpus() {
            let mut blitter = Blitter::new(upload.device(), 2).unwrap();
            let mut readback = Readback::default();
            let mut read = |lease: &Lease| {
                let mut out = I420::empty();
                readback.read(lease, &mut out).unwrap();
                out.y
            };
            upload.put(&picture(1), WHOLE);
            let big = blitter.blit(upload.texture(), SIZE).unwrap().unwrap();
            let before = read(&big);
            let old = Arc::downgrade(&big.0.pool);
            let small = blitter
                .blit(upload.texture(), (1280, 720))
                .unwrap()
                .expect("a texture of the new size");
            assert_eq!(small.0.pool.size, (1280, 720), "{name}");
            assert!(
                read(&big) == before,
                "{name}: the old picture was drawn over"
            );
            drop(big);
            assert!(
                old.upgrade().is_none(),
                "{name}: the old pool outlived its last lease"
            );
            drop(small);
            assert!(
                blitter
                    .blit(upload.texture(), (1280, 720))
                    .unwrap()
                    .is_some(),
                "{name}: the new pool goes on"
            );
        }
    }
}
