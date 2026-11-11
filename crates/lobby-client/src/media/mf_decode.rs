//! The Viewer's H.264 decoder, Media Foundation's. A GPU picture is copied into a staging
//! texture and mapped on the conversion thread: on Windows, GPUI draws only pictures in memory.
//! One the view paints smaller is first scaled down and converted to BGRA on the GPU
//! ([`Scaler`]), so only the pixels on screen are read back.
#![allow(unsafe_code)]

use std::{borrow::Cow, mem::ManuallyDrop, num::NonZeroUsize};

use tracing::{info, warn};
use windows::{
    Win32::{
        Graphics::{
            Direct3D11::{
                D3D11_BOX, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
            },
            Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12},
        },
        Media::MediaFoundation::{
            CLSID_MSH264DecoderMFT, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample, IMFTransform,
            MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
            MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
            MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE,
            MFCreateDXGIDeviceManager, MFCreateMediaType, MFMediaType_Video,
            MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
            MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoArea,
            MFVideoFormat_H264, MFVideoFormat_NV12,
        },
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::Interface,
};

use super::{
    MediaError,
    gpu::{self, Scaler},
    mf::{Com, FRAME_TIME, Platform, memory_sample},
    video::{Nv12Planes, nv12_to_bgra, scaled_for},
};

fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> MediaError {
    move |e| MediaError::Video(format!("decoder {what}: {e}"))
}

struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// `None` where the video processor writes no BGRA: pictures come at their own size.
    scaler: Option<Scaler>,
    /// The picture size shown and the size out the GPU last failed to scale: tried again once
    /// either changes, rather than on every picture.
    refused: Option<((u32, u32), (u32, u32))>,
    #[cfg(test)]
    cannot_scale_to: Option<(u32, u32)>,
    _manager: IMFDXGIDeviceManager,
}

impl Gpu {
    /// A test's stand-in for a GPU that cannot scale to `size`: one without the memory for its
    /// staging texture, or whose video processor refuses that size.
    #[cfg(test)]
    fn fails_at(&self, size: (u32, u32)) -> Result<(), MediaError> {
        if self.cannot_scale_to == Some(size) {
            return Err(MediaError::Video(
                "the test's GPU cannot scale to this".into(),
            ));
        }
        Ok(())
    }

    fn scale(
        &mut self,
        texture: &ID3D11Texture2D,
        shown: (u32, u32),
        size: (u32, u32),
        into: &mut Picture,
    ) -> Result<(), MediaError> {
        #[cfg(test)]
        self.fails_at(size)?;
        let staging = into.staging(self.context.clone(), &self.device, size, Kind::Bgra)?;
        match &mut self.scaler {
            Some(scaler) => scaler.draw(texture, shown, staging),
            None => Err(MediaError::Video("no video processor".into())),
        }
    }

    /// Fails once the device is removed: a reset or a crash of the driver.
    fn alive(&self) -> Result<(), MediaError> {
        // SAFETY: a plain call on a live device.
        unsafe { self.device.GetDeviceRemovedReason() }
            .map_err(|e| MediaError::Video(format!("the GPU was lost: {e}")))
    }

    fn attach(mft: &IMFTransform) -> Result<Gpu, MediaError> {
        let device = gpu::create_device(None)?;
        let mut token = 0u32;
        let mut manager = None;
        // SAFETY: COM calls on live objects; the out pointers are live.
        unsafe {
            let context = device
                .GetImmediateContext()
                .map_err(failed("Direct3D context"))?;
            // The MFT decodes on its device while the conversion thread maps staging textures.
            let lock: ID3D11Multithread = context.cast().map_err(failed("Direct3D lock"))?;
            let _ = lock.SetMultithreadProtected(true);
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(failed("device"))?;
            let manager: IMFDXGIDeviceManager =
                manager.ok_or_else(|| MediaError::Video("no DXGI device manager".into()))?;
            manager
                .ResetDevice(&device, token)
                .map_err(failed("device"))?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .map_err(failed("DXVA"))?;
            let scaler = Scaler::new(&device)
                .inspect_err(|error| warn!(%error, "pictures are read back at their own size"))
                .ok();
            Ok(Gpu {
                device,
                context,
                scaler,
                refused: None,
                #[cfg(test)]
                cannot_scale_to: None,
                _manager: manager,
            })
        }
    }
}

#[derive(Default)]
struct Format {
    /// The decoded size, rounded up to whole macroblocks.
    coded: (u32, u32),
    shown: (u32, u32),
    stride: usize,
    /// The MFT hands over samples of its own; otherwise each output needs one of `sample_size`.
    provides_samples: bool,
    sample_size: u32,
}

/// The newest picture, held to be copied out again at another paint size, with the size it was
/// decoded at: a stream change replaces the decoder's format before the next picture comes.
enum Held {
    /// A GPU picture copied into a texture of the decoder's own. The MFT's sample goes back to
    /// it at once: a held one keeps a surface from its pool, and decoding waits for a free one.
    Gpu {
        texture: ID3D11Texture2D,
        shown: (u32, u32),
    },
    /// The software MFT's one sample, good until the next decode empties it. The length is
    /// taken when the picture comes out.
    Cpu {
        sample: IMFSample,
        len: u32,
        shown: (u32, u32),
        stride: usize,
        coded_h: usize,
    },
}

pub(crate) struct Decoder {
    mft: IMFTransform,
    gpu: Option<Gpu>,
    format: Format,
    sample: Option<IMFSample>,
    held: Option<Held>,
    inputs: i64,
    _platform: Platform,
    _com: Com,
}

impl Decoder {
    /// On the GPU where it decodes H.264, on the CPU otherwise.
    pub fn new() -> Result<Decoder, MediaError> {
        Decoder::start(true)
    }

    #[cfg(test)]
    pub fn cannot_scale_to(&mut self, size: (u32, u32)) {
        if let Some(gpu) = &mut self.gpu {
            gpu.cannot_scale_to = Some(size);
        }
    }

    fn start(on_gpu: bool) -> Result<Decoder, MediaError> {
        let com = Com::init();
        let platform = Platform::start()?;
        // SAFETY: a plain COM call; Media Foundation is started.
        let mft: IMFTransform =
            unsafe { CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                .map_err(|_| MediaError::NoDecoder)?;
        // SAFETY: plain calls on a live MFT.
        let gpu = unsafe {
            let attributes = mft.GetAttributes().map_err(failed("attributes"))?;
            // One picture out for each one in, with none held back.
            attributes
                .SetUINT32(&MF_LOW_LATENCY, 1)
                .map_err(failed("low latency"))?;
            let aware = attributes
                .GetUINT32(&MF_SA_D3D11_AWARE)
                .is_ok_and(|aware| aware != 0);
            (on_gpu && aware)
                .then(|| Gpu::attach(&mft))
                .transpose()
                .unwrap_or_else(|error| {
                    warn!(%error, "the decoder runs on the CPU: the GPU decodes no H.264");
                    None
                })
        };
        let mut decoder = Decoder {
            mft,
            gpu,
            format: Format::default(),
            sample: None,
            held: None,
            inputs: 0,
            _platform: platform,
            _com: com,
        };
        decoder.set_types()?;
        info!(
            hardware = decoder.gpu.is_some(),
            "screen share decoder started"
        );
        Ok(decoder)
    }

    fn set_types(&mut self) -> Result<(), MediaError> {
        // SAFETY: plain calls on a live MFT and media type.
        unsafe {
            let input = MFCreateMediaType().map_err(failed("input type"))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|()| input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))
                .and_then(|()| self.mft.SetInputType(0, &input, 0))
                .map_err(failed("input type"))?;
        }
        self.set_output()
    }

    fn set_output(&mut self) -> Result<(), MediaError> {
        let t = &self.mft;
        // SAFETY: plain calls on a live MFT and its media types. The aperture is read as the
        // blob it is stored as, into a value of its own size.
        unsafe {
            let mut nv12 = None;
            for i in 0.. {
                let Ok(available) = t.GetOutputAvailableType(0, i) else {
                    break;
                };
                if available
                    .GetGUID(&MF_MT_SUBTYPE)
                    .is_ok_and(|s| s == MFVideoFormat_NV12)
                {
                    nv12 = Some(available);
                    break;
                }
            }
            let nv12 =
                nv12.ok_or_else(|| MediaError::Video("the decoder puts out no NV12".into()))?;
            t.SetOutputType(0, &nv12, 0)
                .map_err(failed("output type"))?;
            let size = nv12.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let coded = ((size >> 32) as u32, size as u32);
            let mut area = MFVideoArea::default();
            let shown = nv12
                .GetBlob(
                    &MF_MT_MINIMUM_DISPLAY_APERTURE,
                    std::slice::from_raw_parts_mut(
                        (&raw mut area).cast::<u8>(),
                        size_of::<MFVideoArea>(),
                    ),
                    None,
                )
                .ok()
                .map_or(coded, |()| (area.Area.cx as u32, area.Area.cy as u32));
            let info = t.GetOutputStreamInfo(0).map_err(failed("output stream"))?;
            let own = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            let stride = nv12.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap_or(coded.0);
            self.format = Format {
                coded,
                shown: (shown.0.min(coded.0), shown.1.min(coded.1)),
                stride: stride as usize,
                provides_samples: info.dwFlags & own != 0,
                sample_size: info.cbSize,
            };
        }
        self.sample = if self.format.provides_samples {
            None
        } else {
            Some(memory_sample(self.format.sample_size).map_err(failed("sample"))?)
        };
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), MediaError> {
        self.held = None;
        // SAFETY: a plain call on a live MFT.
        unsafe { self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) }.map_err(failed("flush"))
    }

    /// Decodes `au`, Annex B with its start codes. `true` when it made a picture: the newest is
    /// held for [`Decoder::copy`].
    pub fn decode(&mut self, au: &[u8]) -> Result<bool, MediaError> {
        if matches!(self.held, Some(Held::Cpu { .. })) {
            self.held = None;
        }
        let input = memory_sample(au.len() as u32).map_err(failed("sample"))?;
        // SAFETY: COM calls on live objects. While locked, the buffer holds `au.len()` bytes.
        unsafe {
            let buffer = input.GetBufferByIndex(0).map_err(failed("input"))?;
            let mut bytes = std::ptr::null_mut();
            buffer
                .Lock(&mut bytes, None, None)
                .map_err(failed("input"))?;
            std::ptr::copy_nonoverlapping(au.as_ptr(), bytes, au.len());
            let _ = buffer.Unlock();
            buffer
                .SetCurrentLength(au.len() as u32)
                .and_then(|()| input.SetSampleTime(self.inputs * FRAME_TIME))
                .and_then(|()| input.SetSampleDuration(FRAME_TIME))
                .map_err(failed("input"))?;
            self.inputs += 1;
            self.mft
                .ProcessInput(0, &input, 0)
                .map_err(failed("input"))?;
        }
        let mut made = false;
        loop {
            if let Some(sample) = &self.sample {
                // The software MFT failed (CopyDecodedFrame, E_FAIL) to write into a sample that
                // still held a picture.
                // SAFETY: a plain call on a live sample.
                unsafe {
                    sample
                        .GetBufferByIndex(0)
                        .and_then(|b| b.SetCurrentLength(0))
                }
                .map_err(failed("output sample"))?;
            }
            let ours = self.sample.clone();
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(ours),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: the buffer array lives through the call. Its sample and events are taken
            // back once each afterwards, so their references are released.
            let processed = unsafe { self.mft.ProcessOutput(0, &mut buffers, &mut status) };
            let [buffer] = buffers;
            drop(ManuallyDrop::into_inner(buffer.pEvents));
            let sample = ManuallyDrop::into_inner(buffer.pSample);
            match processed {
                Ok(()) => {
                    let sample = sample
                        .ok_or_else(|| MediaError::Video("an output without a sample".into()))?;
                    self.hold(sample)?;
                    made = true;
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(made),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.set_output()?,
                Err(e) => return Err(failed("output")(e)),
            }
        }
    }

    /// Holds `sample`, the MFT's newest picture. A GPU one is copied out and the sample let go.
    fn hold(&mut self, sample: IMFSample) -> Result<(), MediaError> {
        let shown = self.format.shown;
        // SAFETY: COM calls on live objects. The copy runs on the one immediate context before
        // anything later on it, the MFT's next decode into that texture included.
        unsafe {
            let buffer = sample.GetBufferByIndex(0).map_err(failed("output"))?;
            if let (Some(gpu), Ok(dxgi)) = (&self.gpu, buffer.cast::<IMFDXGIBuffer>()) {
                let mut decoded = std::ptr::null_mut();
                dxgi.GetResource(&ID3D11Texture2D::IID, &mut decoded)
                    .map_err(failed("output texture"))?;
                let decoded = ID3D11Texture2D::from_raw(decoded);
                let index = dxgi
                    .GetSubresourceIndex()
                    .map_err(failed("output texture"))?;
                let size = gpu::texture_size(&decoded);
                let texture = match self.held.take() {
                    Some(Held::Gpu { texture, .. }) if gpu::texture_size(&texture) == size => {
                        texture
                    }
                    _ => gpu::nv12_texture(&gpu.device, size)?,
                };
                gpu.context
                    .CopySubresourceRegion(&texture, 0, 0, 0, 0, &decoded, index, None);
                self.held = Some(Held::Gpu { texture, shown });
            } else {
                let len = buffer.GetCurrentLength().map_err(failed("output"))?;
                self.held = Some(Held::Cpu {
                    sample,
                    len,
                    shown,
                    stride: self.format.stride,
                    coded_h: self.format.coded.1 as usize,
                });
            }
        }
        Ok(())
    }

    /// Copies the held picture out, for a view that paints it at `paint`: scaled down to that on
    /// the GPU where it can be, at its own size otherwise. `false` when none is held.
    pub fn copy(
        &mut self,
        into: &mut Picture,
        paint: Option<(u32, u32)>,
    ) -> Result<bool, MediaError> {
        match (&self.held, &mut self.gpu) {
            (Some(Held::Gpu { texture, shown }), Some(gpu)) => {
                let (w, h) = *shown;
                into.width = w;
                into.height = h;
                if let Some(size) = scaled_for((w, h), paint)
                    && gpu.scaler.is_some()
                    && gpu.refused != Some(((w, h), size))
                {
                    match gpu.scale(texture, (w, h), size, into) {
                        Ok(()) => {
                            gpu.refused = None;
                            return Ok(true);
                        }
                        // A size the video processor refuses, or no memory for its staging
                        // texture, costs the scaling at that size, not the Watch. A lost GPU
                        // reads back nothing.
                        Err(error) => {
                            gpu.alive()?;
                            warn!(%error, ?size, "pictures are read back at their own size");
                            gpu.refused = Some(((w, h), size));
                        }
                    }
                }
                let staging = into.staging(gpu.context.clone(), &gpu.device, (w, h), Kind::Nv12)?;
                let area = D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: w,
                    bottom: h,
                    back: 1,
                };
                // SAFETY: a COM call on live textures; the box lies within both.
                unsafe {
                    gpu.context
                        .CopySubresourceRegion(staging, 0, 0, 0, 0, texture, 0, Some(&area));
                }
            }
            (
                Some(Held::Cpu {
                    sample,
                    len,
                    shown,
                    stride,
                    coded_h,
                }),
                _,
            ) => {
                (into.width, into.height) = *shown;
                // SAFETY: COM calls on a live sample. Its buffer, while locked, holds the `len`
                // bytes the MFT wrote.
                unsafe {
                    let buffer = sample.GetBufferByIndex(0).map_err(failed("output"))?;
                    let mut first = std::ptr::null_mut();
                    buffer
                        .Lock(&mut first, None, None)
                        .map_err(failed("output buffer"))?;
                    let bytes = std::slice::from_raw_parts(first, *len as usize);
                    let whole = bytes.len() >= stride * (coded_h + coded_h / 2);
                    if whole {
                        into.copy_from(bytes, *stride, *coded_h);
                    }
                    let _ = buffer.Unlock();
                    if !whole {
                        return Err(MediaError::Video(
                            "a picture smaller than its format".into(),
                        ));
                    }
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

pub(crate) struct Picture {
    pub width: u32,
    pub height: u32,
    store: Store,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Nv12,
    Bgra,
}

enum Store {
    Empty,
    /// A staging texture of this kind and size, read through its device's context.
    Gpu {
        staging: ID3D11Texture2D,
        context: ID3D11DeviceContext,
        kind: Kind,
        size: (u32, u32),
    },
    /// NV12 in `width`-byte rows, luma then chroma.
    Cpu(Vec<u8>),
}

impl Picture {
    pub fn empty() -> Picture {
        Picture {
            width: 0,
            height: 0,
            store: Store::Empty,
        }
    }

    pub fn hardware(&self) -> bool {
        matches!(self.store, Store::Gpu { .. })
    }

    /// The size [`Picture::to_bgra`] gives.
    pub fn size(&self) -> (u32, u32) {
        match self.store {
            Store::Gpu {
                kind: Kind::Bgra,
                size,
                ..
            } => size,
            _ => (self.width, self.height),
        }
    }

    fn staging(
        &mut self,
        context: ID3D11DeviceContext,
        device: &ID3D11Device,
        size: (u32, u32),
        kind: Kind,
    ) -> Result<&ID3D11Texture2D, MediaError> {
        let fits =
            matches!(&self.store, Store::Gpu { kind: k, size: s, .. } if (*k, *s) == (kind, size));
        if !fits {
            let format = match kind {
                Kind::Nv12 => DXGI_FORMAT_NV12,
                Kind::Bgra => DXGI_FORMAT_B8G8R8A8_UNORM,
            };
            self.store = Store::Gpu {
                staging: gpu::staging(device, size, format)?,
                context,
                kind,
                size,
            };
        }
        match &self.store {
            Store::Gpu { staging, .. } => Ok(staging),
            _ => unreachable!("made above"),
        }
    }

    fn copy_from(&mut self, bytes: &[u8], pitch: usize, coded_h: usize) {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = match std::mem::replace(&mut self.store, Store::Empty) {
            Store::Cpu(out) => out,
            _ => Vec::new(),
        };
        out.resize(w * (h + h / 2), 0);
        let (y, uv) = out.split_at_mut(w * h);
        for (row, out) in y.chunks_exact_mut(w).enumerate() {
            out.copy_from_slice(&bytes[row * pitch..][..w]);
        }
        let chroma = &bytes[pitch * coded_h..];
        for (row, out) in uv.chunks_exact_mut(w).enumerate() {
            out.copy_from_slice(&chroma[row * pitch..][..w]);
        }
        self.store = Store::Cpu(out);
    }

    pub fn to_bgra(&self, bands: NonZeroUsize) -> Result<Vec<u8>, MediaError> {
        let (width, height) = (self.width, self.height);
        let convert = |bytes: &[u8], pitch: usize| {
            let (w, h) = (width as usize, height as usize);
            let (y, uv) = bytes.split_at(pitch * h);
            let uv = &uv[..pitch * (h / 2)];
            // yuvutils takes interleaved chroma only without row padding.
            let tight: Cow<'_, [u8]> = if pitch == w {
                Cow::Borrowed(uv)
            } else {
                let mut tight = Vec::with_capacity(w * (h / 2));
                for row in uv.chunks_exact(pitch) {
                    tight.extend_from_slice(&row[..w]);
                }
                Cow::Owned(tight)
            };
            let planes = Nv12Planes {
                width,
                height,
                y: (y, pitch as u32),
                uv: &tight,
            };
            nv12_to_bgra(&planes, bands)
        };
        match &self.store {
            Store::Gpu {
                staging,
                context,
                kind: Kind::Nv12,
                ..
            } => gpu::read_nv12(context, staging, convert)?,
            Store::Gpu {
                staging,
                context,
                kind: Kind::Bgra,
                size: (w, _),
            } => gpu::read_bgra(context, staging, |bytes, pitch| {
                let row = *w as usize * 4;
                let mut bgra = Vec::with_capacity(row * (bytes.len() / pitch));
                for line in bytes.chunks_exact(pitch) {
                    bgra.extend_from_slice(&line[..row]);
                }
                Ok(bgra)
            })?,
            Store::Cpu(bytes) => convert(bytes, width as usize),
            Store::Empty => Err(MediaError::Video("no picture to convert".into())),
        }
    }
}
