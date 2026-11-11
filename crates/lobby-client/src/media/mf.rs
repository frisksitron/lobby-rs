//! The MFT is asynchronous: it asks for each picture and announces each output with an event. A
//! pump thread waits for those events and takes the outputs; the encoder's own thread submits a
//! picture for each request. Every call into the MFT goes through one lock. An input sample holds
//! its texture's lease until the MFT releases the sample, so the pool never hands out a texture
//! the MFT may still read.
#![allow(unsafe_code)]

use std::{
    collections::VecDeque,
    marker::PhantomData,
    mem::ManuallyDrop,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use lobby_protocol::encoder::{FPS, H264_LEVEL, H264Level};
use tracing::{info, warn};
use windows::{
    Win32::{
        Foundation::{E_NOTIMPL, LUID},
        Graphics::{
            Direct3D11::{ID3D11Device, ID3D11Texture2D},
            Dxgi::{DXGI_ADAPTER_DESC, IDXGIDevice},
        },
        Media::MediaFoundation::{
            CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
            CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
            CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVEncVideoMinQP,
            CODECAPI_AVLowLatencyMode, ICodecAPI, IMFActivate, IMFAsyncCallback,
            IMFAsyncCallback_Impl, IMFAsyncResult, IMFDXGIDeviceManager, IMFMediaEventGenerator,
            IMFSample, IMFShutdown, IMFTransform, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
            METransformHaveOutput, METransformNeedInput, MF_E_SHUTDOWN,
            MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_TYPE, MF_LOW_LATENCY, MF_MT_AVG_BITRATE,
            MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
            MF_MT_MPEG2_LEVEL, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK,
            MF_VERSION, MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer,
            MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFCreateTrackedSample,
            MFMediaType_Video, MFSTARTUP_LITE, MFSampleExtension_CleanPoint, MFShutdown, MFStartup,
            MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE,
            MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_FLUSH,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
            MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
            MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnum2,
            MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
            eAVEncCommonRateControlMode_CBR, eAVEncCommonRateControlMode_PeakConstrainedVBR,
            eAVEncH264VLevel4_2, eAVEncH264VLevel5_2, eAVEncH264VProfile_ConstrainedBase,
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            Variant::VARIANT,
        },
    },
    core::{GUID, Interface, Ref, implement},
};

use super::{MediaError, gpu::Lease};

/// The lowest QP the MFT may pick. Under it NVIDIA's constrained baseline wrote delta pictures
/// that openh264 rejects and ffmpeg decodes (inferred: a CAVLC level_prefix over 15).
const MIN_QP: u32 = 18;
/// One picture at 60 fps, in the MFT's 100 ns units.
pub(crate) const FRAME_TIME: i64 = 10_000_000 / FPS as i64;
/// An MFT that asks for no picture this long has stopped.
const INPUT_WAIT: Duration = Duration::from_secs(1);
const RELEASE_WAIT: Duration = Duration::from_secs(1);

/// Concurrent encoder activation can fail with E_UNEXPECTED on NVIDIA's GPUs.
static ACTIVATION: Mutex<()> = Mutex::new(());

pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

const INTEL: u32 = 0x8086;

fn mf_level(level: H264Level) -> u32 {
    match level {
        H264Level::L4_2 => eAVEncH264VLevel4_2.0 as u32,
        H264Level::L5_2 => eAVEncH264VLevel5_2.0 as u32,
    }
}

fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> MediaError {
    move |e| MediaError::Video(format!("hardware encoder {what}: {e}"))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every holder leaves the data whole: a count, a queue, or the MFT's interfaces.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One encoded picture: an Annex B access unit.
pub(crate) struct Output {
    /// The number [`HardwareEncoder::submit`] gave its picture.
    pub frame: u64,
    pub data: Vec<u8>,
    pub keyframe: bool,
    /// When the MFT handed it over.
    pub at: Instant,
}

pub(crate) struct HardwareEncoder {
    name: String,
    size: (u32, u32),
    session: Session,
    next: u64,
    _platform: Platform,
    _com: Com,
}

impl HardwareEncoder {
    pub fn new(
        device: &ID3D11Device,
        size: (u32, u32),
        bps: u32,
        wake: Wake,
    ) -> Result<HardwareEncoder, MediaError> {
        let com = Com::init();
        let platform = Platform::start()?;
        let (session, name) = Session::start(device, size, bps, &wake)?;
        Ok(HardwareEncoder {
            name,
            size,
            session,
            next: 0,
            _platform: platform,
            _com: com,
        })
    }

    /// The MFT's friendly name, as its maker gives it.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The MFT's first output is a keyframe.
    pub fn submit(&mut self, lease: &Lease, idr: bool) -> Result<u64, MediaError> {
        let frame = self.next;
        let session = &self.session;
        session.shared.request(INPUT_WAIT)?;
        let sample = sample(lease, frame, &session.shared)?;
        let mft = lock(&session.mft);
        if idr {
            set(
                &mft.codec,
                &CODECAPI_AVEncVideoForceKeyFrame,
                "keyframe",
                1u32,
            )?;
        }
        // SAFETY: a plain call on a live MFT, which takes its own reference on the sample.
        unsafe { mft.transform.ProcessInput(0, &sample, 0) }.map_err(failed("input"))?;
        self.next += 1;
        Ok(frame)
    }

    /// The next picture the MFT encoded, if one is ready; the MFT's error once it failed.
    pub fn take(&mut self) -> Result<Option<Output>, MediaError> {
        let mut s = self.session.shared.lock();
        match s.outputs.pop_front() {
            Some(output) => Ok(Some(output)),
            None => s.failed.clone().map_or(Ok(None), Err),
        }
    }
}

/// One MFT and its pump. Dropping it drops what the MFT holds, shuts the MFT down and joins its
/// pump.
struct Session {
    mft: Arc<Mutex<Mft>>,
    shared: Arc<Shared>,
    pump: Option<JoinHandle<()>>,
}

impl Session {
    /// The first hardware H.264 encoder on `device`'s adapter, running at `bps`, and its name.
    fn start(
        device: &ID3D11Device,
        size: (u32, u32),
        bps: u32,
        wake: &Wake,
    ) -> Result<(Session, String), MediaError> {
        let started = Instant::now();
        let (activate, vendor) = find(device)?.ok_or(MediaError::NoHardwareEncoder)?;
        let name = friendly_name(&activate);
        let control = RateControl::for_vendor(vendor);
        let mft = Mft::new(activate, device, size, bps, control)?;
        let events = Events(mft.transform.cast().map_err(failed("events"))?);
        // SAFETY: plain calls on a live MFT whose types are set.
        unsafe {
            let t = &mft.transform;
            t.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(failed("start"))?;
            t.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(failed("start"))?;
        }
        let mft = Arc::new(Mutex::new(mft));
        let shared = Arc::new(Shared::default());
        let (pump_mft, pump_shared, wake) = (mft.clone(), shared.clone(), wake.clone());
        let pump = std::thread::Builder::new()
            .name("lobby-video-mft".into())
            .spawn(move || pump(&pump_mft, &events, &pump_shared, &wake))
            .map_err(|e| MediaError::Video(format!("hardware encoder pump: {e}")))?;
        info!(
            encoder = %name,
            rate_control = ?control,
            width = size.0,
            height = size.1,
            mean_kbps = bps / 1000,
            setup_ms = started.elapsed().as_millis() as u64,
            "hardware encoder started"
        );
        let session = Session {
            mft,
            shared,
            pump: Some(pump),
        };
        Ok((session, name))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        lock(&self.mft).stop();
        if let Some(pump) = self.pump.take()
            && pump.join().is_err()
        {
            warn!("the hardware encoder's pump panicked");
        }
        let held = self.shared.released(RELEASE_WAIT);
        if held > 0 {
            warn!(held, "the hardware encoder kept textures after it stopped");
        }
    }
}

/// The rate control mode each maker's MFT was measured to keep its bitrate in: NVIDIA's in CBR,
/// Intel's UHD 770 in peak-constrained VBR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RateControl {
    Constant,
    /// The peak is the mean, so the mean is also a ceiling.
    PeakConstrained,
}

impl RateControl {
    /// By the adapter's PCI vendor ID. AMD's MFT was never measured, so it keeps CBR.
    fn for_vendor(vendor: u32) -> RateControl {
        if vendor == INTEL {
            RateControl::PeakConstrained
        } else {
            RateControl::Constant
        }
    }

    /// Sets the MFT's mode and gives it `bps`, before it starts.
    fn start(self, codec: &ICodecAPI, bps: u32) -> Result<(), MediaError> {
        let mode = match self {
            RateControl::Constant => eAVEncCommonRateControlMode_CBR,
            RateControl::PeakConstrained => eAVEncCommonRateControlMode_PeakConstrainedVBR,
        };
        set(
            codec,
            &CODECAPI_AVEncCommonRateControlMode,
            "rate control",
            mode.0 as u32,
        )?;
        set(codec, &CODECAPI_AVEncCommonMeanBitRate, "bitrate", bps)?;
        match self {
            RateControl::Constant => Ok(()),
            RateControl::PeakConstrained => {
                set(codec, &CODECAPI_AVEncCommonMaxBitRate, "peak bitrate", bps)
            }
        }
    }
}

/// The MFT and what it was set up with. The pump and the encoder's thread both call it, through
/// the one `Mutex<Mft>`.
struct Mft {
    activate: IMFActivate,
    transform: IMFTransform,
    codec: ICodecAPI,
    shutdown: IMFShutdown,
    _manager: IMFDXGIDeviceManager,
    control: RateControl,
    /// The MFT hands over samples of its own; otherwise each output needs one of this size.
    provides_samples: bool,
    output_size: u32,
    /// Flushed and shut down: nothing calls it any more.
    stopped: bool,
}

// SAFETY: a hardware MFT is asynchronous, which makes it free threaded: Media Foundation's own
// pipeline calls one from its work-queue threads. Here every call goes through `Mutex<Mft>`.
unsafe impl Send for Mft {}

impl Mft {
    fn new(
        activate: IMFActivate,
        device: &ID3D11Device,
        size: (u32, u32),
        bps: u32,
        control: RateControl,
    ) -> Result<Mft, MediaError> {
        let mut token = 0u32;
        let mut manager = None;
        // SAFETY: the out pointers are live; the manager takes its own reference on the device.
        let manager = unsafe {
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(failed("device"))?;
            let manager: IMFDXGIDeviceManager =
                manager.ok_or_else(|| MediaError::Video("no DXGI device manager".into()))?;
            manager
                .ResetDevice(device, token)
                .map_err(failed("device"))?;
            manager
        };
        let transform: IMFTransform = {
            let _turn = lock(&ACTIVATION);
            // SAFETY: a plain call on a live activation object.
            unsafe { activate.ActivateObject() }.map_err(failed("activation"))?
        };
        let parts = transform
            .cast::<ICodecAPI>()
            .and_then(|codec| Ok((codec, transform.cast::<IMFShutdown>()?)));
        let (codec, shutdown) = match parts {
            Ok(parts) => parts,
            Err(e) => {
                // SAFETY: as above.
                let _ = unsafe { activate.ShutdownObject() };
                return Err(failed("interfaces")(e));
            }
        };
        let mut mft = Mft {
            activate,
            transform,
            codec,
            shutdown,
            _manager: manager,
            control,
            provides_samples: false,
            output_size: 0,
            stopped: false,
        };
        mft.configure(size, bps)?;
        Ok(mft)
    }

    fn configure(&mut self, (w, h): (u32, u32), bps: u32) -> Result<(), MediaError> {
        let frame_size = (u64::from(w) << 32) | u64::from(h);
        let frame_rate = (u64::from(FPS) << 32) | 1;
        let t = &self.transform;
        // SAFETY: COM calls on live objects; the manager outlives the MFT's use of it (it is
        // released after the MFT shuts down), and the stream info is a plain out value.
        unsafe {
            let attributes = t.GetAttributes().map_err(failed("attributes"))?;
            attributes
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(failed("unlock"))?;
            attributes
                .SetUINT32(&MF_LOW_LATENCY, 1)
                .map_err(failed("low latency"))?;
            t.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, self._manager.as_raw() as usize)
                .map_err(failed("device"))?;
            let c = &self.codec;
            set(c, &CODECAPI_AVLowLatencyMode, "low latency", true)?;
            self.control.start(c, bps)?;
            // Keyframes only when asked for. NVIDIA's encoder makes no B-frames and refuses the
            // setting.
            set(c, &CODECAPI_AVEncMPVGOPSize, "GOP size", 0u32)?;
            let _ = set(c, &CODECAPI_AVEncMPVDefaultBPictureCount, "B-frames", 0u32);
            if let Err(error) = set(c, &CODECAPI_AVEncVideoMinQP, "QP floor", MIN_QP) {
                warn!(%error, "the hardware encoder took no QP floor");
            }

            let output = MFCreateMediaType().map_err(failed("output type"))?;
            let profile = eAVEncH264VProfile_ConstrainedBase.0 as u32;
            output
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|()| output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))
                .and_then(|()| output.SetUINT32(&MF_MT_AVG_BITRATE, bps))
                .and_then(|()| output.SetUINT64(&MF_MT_FRAME_SIZE, frame_size))
                .and_then(|()| output.SetUINT64(&MF_MT_FRAME_RATE, frame_rate))
                .and_then(|()| {
                    let progressive = MFVideoInterlace_Progressive.0 as u32;
                    output.SetUINT32(&MF_MT_INTERLACE_MODE, progressive)
                })
                .and_then(|()| output.SetUINT32(&MF_MT_MPEG2_PROFILE, profile))
                .and_then(|()| output.SetUINT32(&MF_MT_MPEG2_LEVEL, mf_level(H264_LEVEL)))
                .and_then(|()| t.SetOutputType(0, &output, 0))
                .map_err(failed("output type"))?;

            let mut input = None;
            for i in 0.. {
                let Ok(available) = t.GetInputAvailableType(0, i) else {
                    break;
                };
                if available
                    .GetGUID(&MF_MT_SUBTYPE)
                    .is_ok_and(|s| s == MFVideoFormat_NV12)
                {
                    input = Some(available);
                    break;
                }
            }
            let input = input
                .ok_or_else(|| MediaError::Video("the hardware encoder takes no NV12".into()))?;
            input
                .SetUINT64(&MF_MT_FRAME_SIZE, frame_size)
                .and_then(|()| input.SetUINT64(&MF_MT_FRAME_RATE, frame_rate))
                .and_then(|()| t.SetInputType(0, &input, 0))
                .map_err(failed("input type"))?;

            let info = t.GetOutputStreamInfo(0).map_err(failed("output stream"))?;
            self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            self.output_size = info.cbSize;
        }
        Ok(())
    }

    /// The output the MFT announced; `None` for a format change, or once it stopped.
    fn output(&self) -> Result<Option<(u64, Vec<u8>, bool)>, MediaError> {
        if self.stopped {
            return Ok(None);
        }
        let ours = if self.provides_samples {
            None
        } else {
            Some(memory_sample(self.output_size).map_err(failed("output sample"))?)
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(ours),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: the buffer array lives through the call. Its sample and events are taken back
        // once each afterwards, so their references are released.
        let processed = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
        let [buffer] = buffers;
        drop(ManuallyDrop::into_inner(buffer.pEvents));
        let sample = ManuallyDrop::into_inner(buffer.pSample);
        match processed {
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // SAFETY: plain calls on a live MFT.
                unsafe {
                    let changed = self
                        .transform
                        .GetOutputAvailableType(0, 0)
                        .map_err(failed("output change"))?;
                    self.transform
                        .SetOutputType(0, &changed, 0)
                        .map_err(failed("output change"))?;
                }
                return Ok(None);
            }
            Err(e) => return Err(failed("output")(e)),
            Ok(()) => {}
        }
        let sample =
            sample.ok_or_else(|| MediaError::Video("an output without a sample".into()))?;
        // SAFETY: COM calls on a live sample. While locked, the buffer holds `len` bytes, copied
        // out before it is unlocked.
        unsafe {
            let time = sample.GetSampleTime().map_err(failed("output time"))?;
            let keyframe = sample
                .GetUINT32(&MFSampleExtension_CleanPoint)
                .is_ok_and(|clean| clean == 1);
            let buffer = sample
                .ConvertToContiguousBuffer()
                .map_err(failed("output buffer"))?;
            let (mut bytes, mut len) = (std::ptr::null_mut(), 0u32);
            buffer
                .Lock(&mut bytes, None, Some(&mut len))
                .map_err(failed("output buffer"))?;
            let data = std::slice::from_raw_parts(bytes, len as usize).to_vec();
            let _ = buffer.Unlock();
            let frame = (time + FRAME_TIME / 2) / FRAME_TIME;
            Ok(Some((frame as u64, data, keyframe)))
        }
    }

    /// Drops every sample the MFT holds and shuts it down, which wakes the pump.
    fn stop(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        // SAFETY: plain calls on a live MFT, which is not called again.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self.shutdown.Shutdown();
        }
    }
}

impl Drop for Mft {
    fn drop(&mut self) {
        self.stop();
        // SAFETY: a plain call on a live activation object.
        let _ = unsafe { self.activate.ShutdownObject() };
    }
}

fn set(
    codec: &ICodecAPI,
    api: &GUID,
    what: &'static str,
    value: impl Into<VARIANT>,
) -> Result<(), MediaError> {
    // SAFETY: the GUID and the value are live for the call.
    unsafe { codec.SetValue(api, &value.into()) }.map_err(failed(what))
}

pub(super) fn memory_sample(size: u32) -> windows::core::Result<IMFSample> {
    // SAFETY: plain calls; the sample takes its own reference on the buffer.
    unsafe {
        let sample = MFCreateSample()?;
        let buffer = MFCreateMemoryBuffer(size)?;
        sample.AddBuffer(&buffer)?;
        Ok(sample)
    }
}

/// An input sample of `lease`'s texture. It holds a clone of the lease until the MFT and this
/// encoder have both let go of it.
fn sample(lease: &Lease, frame: u64, shared: &Arc<Shared>) -> Result<IMFSample, MediaError> {
    shared.lock().held += 1;
    let release: IMFAsyncCallback = Release {
        lease: Mutex::new(Some(lease.clone())),
        shared: shared.clone(),
    }
    .into();
    // SAFETY: COM calls on live objects. The buffer takes its own reference on the texture, the
    // sample on the buffer and on the callback.
    unsafe {
        let tracked = MFCreateTrackedSample().map_err(failed("sample"))?;
        let sample: IMFSample = tracked.cast().map_err(failed("sample"))?;
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, lease.texture(), 0, false)
            .map_err(failed("sample"))?;
        sample.AddBuffer(&buffer).map_err(failed("sample"))?;
        sample
            .SetSampleTime(frame as i64 * FRAME_TIME)
            .and_then(|()| sample.SetSampleDuration(FRAME_TIME))
            .map_err(failed("sample"))?;
        tracked
            .SetAllocator(&release, None)
            .map_err(failed("sample"))?;
        Ok(sample)
    }
}

/// Hands a texture's lease back once nothing holds the sample that carries it: the tracked sample
/// calls this when its last other reference goes, and drops it after.
#[implement(IMFAsyncCallback)]
struct Release {
    lease: Mutex<Option<Lease>>,
    shared: Arc<Shared>,
}

impl Release {
    fn release(&self) {
        // The lease goes back under the count's lock, so `held` never disagrees with the pool.
        let mut s = self.shared.lock();
        if lock(&self.lease).take().is_some() {
            s.held -= 1;
            drop(s);
            self.shared.changed.notify_all();
        }
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        self.release();
    }
}

impl IMFAsyncCallback_Impl for Release_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, _result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        self.release();
        Ok(())
    }
}

/// What the pump and the encoder's thread share, besides the MFT.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    /// Pictures the MFT asked for and was not given yet.
    wanted: u32,
    outputs: VecDeque<Output>,
    /// Input samples the MFT has not let go of.
    held: usize,
    /// Why the pump stopped, when the MFT failed.
    failed: Option<MediaError>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn change(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.lock());
        self.changed.notify_all();
    }

    /// Takes one of the MFT's requests for a picture, waiting up to `wait` for one.
    fn request(&self, wait: Duration) -> Result<(), MediaError> {
        let deadline = Instant::now() + wait;
        let mut s = self.lock();
        loop {
            if let Some(e) = &s.failed {
                return Err(e.clone());
            }
            if s.wanted > 0 {
                s.wanted -= 1;
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(MediaError::Video(
                    "the hardware encoder stopped taking pictures".into(),
                ));
            }
            s = self
                .changed
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Waits up to `wait` for every input sample to be let go of; the number still held.
    fn released(&self, wait: Duration) -> usize {
        let deadline = Instant::now() + wait;
        let mut s = self.lock();
        while s.held > 0 {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            s = self
                .changed
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        s.held
    }
}

/// The MFT's event queue, which the pump reads without the lock: an asynchronous MFT is driven by
/// reading its events while other calls go on, and shutting it down wakes a read that waits.
struct Events(IMFMediaEventGenerator);

// SAFETY: as for `Mft`; the queue is the MFT's own, made to be read from another thread.
unsafe impl Send for Events {}

/// Counts the MFT's requests for pictures and takes its outputs, until it shuts down or fails.
fn pump(mft: &Mutex<Mft>, events: &Events, shared: &Shared, wake: &Wake) {
    let _com = Com::init();
    let fail = |error: MediaError| {
        warn!(%error, "the hardware encoder failed");
        shared.change(|s| s.failed = Some(error));
        wake();
    };
    loop {
        // SAFETY: a plain call on a live queue. It blocks until an event comes or the MFT shuts
        // down.
        let event = match unsafe { events.0.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) } {
            Ok(event) => event,
            Err(e) if e.code() == MF_E_SHUTDOWN => return,
            Err(e) => return fail(failed("events")(e)),
        };
        // SAFETY: plain calls on a live event.
        let (kind, status) = unsafe { (event.GetType(), event.GetStatus()) };
        if let Ok(status) = status
            && status.is_err()
        {
            return fail(failed("event")(status.into()));
        }
        let kind = kind.map_or(MF_EVENT_TYPE(0), |k| MF_EVENT_TYPE(k as i32));
        if kind == METransformNeedInput {
            shared.change(|s| s.wanted += 1);
        } else if kind == METransformHaveOutput {
            match lock(mft).output() {
                Ok(Some((frame, data, keyframe))) => {
                    let output = Output {
                        frame,
                        data,
                        keyframe,
                        at: Instant::now(),
                    };
                    shared.change(|s| s.outputs.push_back(output));
                    wake();
                }
                Ok(None) => {}
                Err(error) => return fail(error),
            }
        }
    }
}

fn adapter(device: &ID3D11Device) -> Result<DXGI_ADAPTER_DESC, MediaError> {
    // SAFETY: COM calls on a live device.
    unsafe {
        device
            .cast::<IDXGIDevice>()
            .and_then(|d| d.GetAdapter())
            .and_then(|a| a.GetDesc())
            .map_err(failed("adapter"))
    }
}

/// The first hardware H.264 encoder on `device`'s adapter that takes NV12, if there is one, and
/// the adapter's PCI vendor ID.
fn find(device: &ID3D11Device) -> Result<Option<(IMFActivate, u32)>, MediaError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let adapter = adapter(device)?;
    // SAFETY: COM calls on live objects. The LUID is read as its 8 bytes. MFTEnum2 hands back an
    // array of `count` activation objects the caller owns: each is moved out once, then the array
    // is freed.
    unsafe {
        let luid = adapter.AdapterLuid;
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1).map_err(failed("search"))?;
        let attributes =
            attributes.ok_or_else(|| MediaError::Video("no Media Foundation attributes".into()))?;
        let luid_bytes =
            std::slice::from_raw_parts((&raw const luid).cast::<u8>(), size_of::<LUID>());
        attributes
            .SetBlob(&MFT_ENUM_ADAPTER_LUID, luid_bytes)
            .map_err(failed("search"))?;
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &attributes,
            &mut list,
            &mut count,
        )
        .map_err(failed("search"))?;
        let mut first = None;
        for i in 0..count as usize {
            let activate: Option<IMFActivate> = std::ptr::read(list.add(i));
            first = first.or(activate);
        }
        CoTaskMemFree(Some(list.cast_const().cast()));
        Ok(first.map(|activate| (activate, adapter.VendorId)))
    }
}

fn friendly_name(activate: &IMFActivate) -> String {
    let key = &MFT_FRIENDLY_NAME_Attribute;
    // SAFETY: the buffer has room for the name and its terminator.
    unsafe {
        let name = activate.GetStringLength(key).and_then(|len| {
            let mut buf = vec![0u16; len as usize + 1];
            activate.GetString(key, &mut buf, None)?;
            Ok(String::from_utf16_lossy(&buf[..len as usize]))
        });
        name.unwrap_or_else(|_| "hardware H.264 encoder".into())
    }
}

/// The name of the hardware H.264 encoder on the default adapter's GPU, the one a capture's
/// device is on, if it has one.
pub fn hardware_encoder() -> Option<String> {
    let _com = Com::init();
    let _platform = Platform::start().ok()?;
    let device = super::gpu::create_device(None).ok()?;
    find(&device)
        .ok()?
        .map(|(activate, _)| friendly_name(&activate))
}

/// Media Foundation, started for as long as this lives. It counts its starts.
pub(crate) struct Platform;

impl Platform {
    pub fn start() -> Result<Platform, MediaError> {
        // SAFETY: a plain call, paired with `MFShutdown` on drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }
            .map_err(|e| MediaError::Video(format!("Media Foundation: {e}")))?;
        Ok(Platform)
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        // SAFETY: pairs the start above.
        let _ = unsafe { MFShutdown() };
    }
}

/// COM on this thread, in the multithreaded apartment, for as long as this lives. Not `Send`:
/// it ends on the thread that began it.
pub(crate) struct Com(bool, PhantomData<*const ()>);

impl Com {
    pub fn init() -> Com {
        // SAFETY: a plain call; a success is paired with `CoUninitialize` on drop.
        let ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Com(ok, PhantomData)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: pairs a successful `CoInitializeEx` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::media::{
        gpu::{Blitter, Upload, adapters},
        pattern::noise_pixel,
        watch::nal_types,
    };

    /// At this size the test pattern's noise fills 2 Mbps on NVIDIA's and Intel's encoders, and
    /// at 0.5 Mbps stays above their coarsest quality. Full-range noise does not: Intel's sent
    /// 5.4 Mbps whatever it was asked for.
    const SIZE: (u32, u32) = (640, 360);
    /// Long enough for any output; an output that never comes fails the test, not a timer.
    const OUTPUT_WAIT: Duration = Duration::from_secs(5);

    /// A GPU that draws pictures into a pool as a capture does, with its hardware encoder's
    /// name. Each picture is fresh noise of the test pattern's kind, as costly as busy real
    /// content, so the bytes follow the target.
    struct Gpu {
        name: String,
        upload: Upload,
        blitter: Blitter,
        picture: Vec<u8>,
        noise: u32,
    }

    /// One for each adapter whose GPU draws pictures and has a hardware H.264 encoder: each
    /// maker's encoder runs in its own rate control mode. The others are skipped, and the test
    /// output says why.
    fn gpus(textures: usize) -> Vec<Gpu> {
        let _com = Com::init();
        let Ok(_platform) = Platform::start() else {
            eprintln!("no Media Foundation here: nothing was tested");
            return Vec::new();
        };
        let found: Vec<Gpu> = adapters()
            .into_iter()
            .filter(|(.., software)| !software)
            .filter_map(|(adapter_name, adapter, _)| {
                let drawing = Upload::new(Some(&adapter), SIZE)
                    .and_then(|upload| Ok((Blitter::new(upload.device(), textures)?, upload)));
                let (blitter, upload) = drawing
                    .inspect_err(|e| eprintln!("skipped {adapter_name}: {e}"))
                    .ok()?;
                let Some((encoder, _)) = find(upload.device()).unwrap() else {
                    eprintln!("skipped {adapter_name}: no hardware H.264 encoder");
                    return None;
                };
                Some(Gpu {
                    name: friendly_name(&encoder),
                    upload,
                    blitter,
                    picture: vec![0; (SIZE.0 * SIZE.1 * 4) as usize],
                    noise: 0x2545_f491,
                })
            })
            .collect();
        if found.is_empty() {
            eprintln!("no GPU here has a hardware H.264 encoder: nothing was tested");
        }
        found
    }

    impl Gpu {
        fn draw(&mut self) -> Option<Lease> {
            for px in self.picture.chunks_exact_mut(4) {
                px.copy_from_slice(&noise_pixel(&mut self.noise));
            }
            self.upload.put(&self.picture, (0, 0, SIZE.0, SIZE.1));
            self.blitter.blit(self.upload.texture(), SIZE).unwrap()
        }

        /// Every texture left in the pool, held until the leases drop.
        fn drain(&mut self) -> Vec<Lease> {
            std::iter::from_fn(|| self.draw()).collect()
        }

        /// The textures in the pool and those `encoder` holds, counted again until the MFT lets
        /// go of none while they are counted.
        fn count(&mut self, encoder: &HardwareEncoder) -> (usize, usize) {
            loop {
                let held = shared(encoder).lock().held;
                let free = self.drain().len();
                if shared(encoder).lock().held == held {
                    return (free, held);
                }
            }
        }

        fn encoder(&self) -> HardwareEncoder {
            let device = self.upload.device();
            HardwareEncoder::new(device, SIZE, 2_000_000, Arc::new(|| {})).unwrap()
        }
    }

    fn shared(encoder: &HardwareEncoder) -> &Shared {
        &encoder.session.shared
    }

    /// The next picture the encoder finishes.
    fn output(encoder: &mut HardwareEncoder) -> Output {
        let deadline = Instant::now() + OUTPUT_WAIT;
        loop {
            if let Some(output) = encoder.take().unwrap() {
                return output;
            }
            let s = shared(encoder).lock();
            if s.outputs.is_empty() {
                let now = Instant::now();
                assert!(now < deadline, "no output");
                drop(shared(encoder).changed.wait_timeout(s, deadline - now));
            }
        }
    }

    fn encode(
        gpu: &mut Gpu,
        encoder: &mut HardwareEncoder,
        count: usize,
        idr: impl Fn(usize) -> bool,
    ) -> Vec<Output> {
        (0..count)
            .map(|n| {
                let lease = gpu.draw().expect("a free texture");
                let frame = encoder.submit(&lease, idr(n)).unwrap();
                let out = output(encoder);
                assert_eq!(out.frame, frame, "one output for each picture, in order");
                out
            })
            .collect()
    }

    const IDR: u8 = 5;

    #[test]
    fn the_first_output_is_a_constrained_baseline_sps_at_the_advertised_level() {
        for mut gpu in gpus(3) {
            let mut encoder = gpu.encoder();
            let first = encode(&mut gpu, &mut encoder, 1, |_| false).remove(0);
            let name = &gpu.name;
            assert!(first.keyframe, "{name}: the first picture is a keyframe");
            let id = crate::media::watch::sps_profile_level(&first.data).expect("an SPS");
            // The profile and level, and constraint_set1 (constrained baseline).
            let kept = |id: u32| id & 0xff_40_ff;
            assert_eq!(
                kept(id),
                kept(H264_LEVEL.profile_level_id()),
                "{name}: {id:#08x}"
            );
        }
    }

    /// A GPU that cannot scale a picture to the size the view paints it at, for want of memory or
    /// because its video processor refuses the size, reads it back whole: the Watch goes on.
    #[test]
    fn a_picture_the_gpu_cannot_scale_is_read_back_whole() {
        use crate::media::mf_decode::{Decoder, Picture};

        let half = (SIZE.0 / 2, SIZE.1 / 2);
        for mut gpu in gpus(3) {
            let name = gpu.name.clone();
            let mut encoder = gpu.encoder();
            let first = encode(&mut gpu, &mut encoder, 1, |_| false).remove(0);
            let mut decoder = Decoder::new().unwrap();
            decoder.cannot_scale_to(half);
            assert!(decoder.decode(&first.data).expect(&name), "{name}");
            let mut picture = Picture::empty();
            assert!(
                decoder.copy(&mut picture, Some(half)).expect(&name),
                "{name}"
            );
            assert_eq!(picture.size(), SIZE, "{name}");
            let bgra = picture.to_bgra(std::num::NonZeroUsize::MIN).expect(&name);
            assert_eq!(bgra.len() as u32, SIZE.0 * SIZE.1 * 4, "{name}");
        }
    }

    /// One size the GPU cannot scale to costs the scaling at that size only: a view painted at
    /// another size gets its picture scaled again.
    #[test]
    fn a_size_the_gpu_cannot_scale_to_leaves_the_others_scaled() {
        use crate::media::mf_decode::{Decoder, Picture};

        let (half, third) = ((SIZE.0 / 2, SIZE.1 / 2), (SIZE.0 / 3, SIZE.1 / 3));
        for mut gpu in gpus(3) {
            let name = gpu.name.clone();
            let mut encoder = gpu.encoder();
            let first = encode(&mut gpu, &mut encoder, 1, |_| false).remove(0);
            let mut decoder = Decoder::new().unwrap();
            decoder.cannot_scale_to(half);
            assert!(decoder.decode(&first.data).expect(&name), "{name}");
            for (paint, out_size) in [(half, SIZE), (third, third), (half, SIZE), (third, third)] {
                let mut picture = Picture::empty();
                assert!(
                    decoder.copy(&mut picture, Some(paint)).expect(&name),
                    "{name}"
                );
                assert_eq!(picture.size(), out_size, "{name}, painted at {paint:?}");
            }
        }
    }

    #[test]
    fn a_keyframe_comes_out_on_exactly_the_picture_it_was_asked_for() {
        for mut gpu in gpus(3) {
            let mut encoder = gpu.encoder();
            let outputs = encode(&mut gpu, &mut encoder, 30, |n| n == 12);
            let flagged: Vec<usize> = (0..30).filter(|n| outputs[*n].keyframe).collect();
            let idrs: Vec<usize> = (0..30)
                .filter(|n| nal_types(&outputs[*n].data).contains(&IDR))
                .collect();
            assert_eq!(flagged, [0, 12], "{}: keyframes", gpu.name);
            assert_eq!(idrs, [0, 12], "{}: IDR pictures", gpu.name);
        }
    }

    /// NVIDIA's MFT keeps its newest picture until the next one comes, past its output; Intel's
    /// lets go of it around its output (measured). Either way a texture stays out of the pool
    /// while the MFT holds it, and every texture is back once the encoder is gone.
    #[test]
    fn a_texture_goes_back_to_its_pool_only_once_the_encoder_lets_go_of_it() {
        for mut gpu in gpus(2) {
            let mut encoder = gpu.encoder();
            for n in 0..2 {
                let lease = gpu.draw().expect("a free texture");
                encoder.submit(&lease, false).unwrap();
                drop(lease);
                output(&mut encoder);
                let (free, held) = gpu.count(&encoder);
                assert_eq!(
                    free + held,
                    2,
                    "{}, picture {n}: {held} held by the MFT, {free} in the pool",
                    gpu.name
                );
            }
            drop(encoder);
            let back = gpu.drain().len();
            assert_eq!(
                back, 2,
                "{}: both textures back once the encoder is gone",
                gpu.name
            );
        }
    }
}
