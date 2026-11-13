//! Lobby's streams on a sound server that speaks the PulseAudio protocol: PulseAudio itself, or
//! PipeWire's `pipewire-pulse`. cpal's Pulse host names every stream after itself
//! (`cpal-pulseaudio-<pid>`), gives it no media name or role and has no way to be told more, so
//! the streams of a call and of the interface sounds are opened here instead, with what the
//! desktop's sound settings and volume mixer read: the app's name, icon and id, what the stream is
//! for, and its role (`phone` lets the desktop pause or duck music for a call, and GNOME show the
//! microphone indicator; `event` marks an interface sound). cpal still lists the devices and says
//! which one to open.
//!
//! The server converts: the streams ask for 48 kHz f32, stereo for playing and mono for recording,
//! whatever the device is.
//!
//! A server can accept a connection and then stop answering. The `pulseaudio` crate waits for
//! an answer without a limit: its handshake reads a blocking socket, and every later reply is a
//! future its reactor thread completes only when the server speaks. So every wait here has a
//! deadline and runs on the thread that asked, and Lobby keeps a second handle on each
//! connection's socket: shutting it down ends the handshake, every pending reply and the
//! reactor thread itself, and lets go of the stream's callbacks.

use std::{
    any::Any,
    ffi::CString,
    future::Future,
    net::Shutdown,
    os::unix::net::UnixStream,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

use pulseaudio::{
    AsPlaybackSource as _, Client, ClientError,
    protocol::{
        ChannelMap, PlaybackStreamParams, Prop, Props, PulseError, RecordStreamParams,
        SampleFormat, SampleSpec,
        stream::{BufferAttr, StreamFlags},
    },
};
use socket2::{Domain, SockAddr, Socket, Type};
use tracing::{debug, warn};

use super::{DEVICE_START_TIMEOUT, FRAME, MediaError, SAMPLE_RATE, audio::Purpose};
use crate::APP_ID;

/// How often a stream is asked whether the server still has it.
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// How long the server has to answer that question. A server that does not has lost the stream.
const ANSWER_WITHIN: Duration = Duration::from_secs(2);
/// The longest closing a stream waits for its watcher, which ends at once when told to.
const CLOSE_WITHIN: Duration = Duration::from_millis(500);
/// How long the server has to answer before cpal is let near it (`host`): cpal's own limit.
const PROBE_WITHIN: Duration = Duration::from_secs(2);
/// What the output callback renders at once: 100 ms of mono, far beyond any request.
const SCRATCH: usize = SAMPLE_RATE as usize / 10;
const NAME: &str = "Lobby";
const BYTES_PER_SAMPLE: usize = size_of::<f32>();
const PLAY_CHANNELS: usize = 2;

impl Purpose {
    fn media_name(self, input: bool) -> &'static str {
        match (self, input) {
            (Purpose::Voice, true) => "Microphone",
            (Purpose::Voice, false) => "Voice",
            (Purpose::Test, _) => "Microphone test",
            (Purpose::Interface, _) => "Sounds",
        }
    }

    fn role(self) -> &'static str {
        match self {
            Purpose::Voice => "phone",
            Purpose::Test => "test",
            Purpose::Interface => "event",
        }
    }
}

/// The name of the sink or source a cpal device is on a Pulse server, or None for another host.
pub(super) fn name_of(device: &cpal::Device) -> Option<String> {
    use cpal::traits::DeviceTrait as _;
    let id = device.id().ok()?;
    (id.host() == cpal::HostId::PulseAudio).then(|| id.id().to_owned())
}

/// cpal's default host, without cpal's unbounded wait. cpal connects to a Pulse server on a
/// thread of its own and leaves that thread behind for good when the server does not answer
/// within two seconds, at every call. So a connection Lobby can cut asks first, and when the
/// server does not answer in time cpal's ALSA host is taken, as cpal itself would. What is left
/// is the moment between the two: a server that stops answering just then still costs one of
/// cpal's threads, and the next call asks first again.
///
/// Every Pulse host is the one [`shared`] host: a dropped cpal Pulse host keeps its connection
/// and its reactor thread until the server next writes to it, which an idle connection never
/// gets, so a host per Join, Leave and interface sound piled up.
pub(super) fn host() -> Arc<cpal::Host> {
    use cpal::traits::HostTrait as _;
    if let Some(alsa) = alsa_if_unanswered() {
        return Arc::new(alsa);
    }
    let host = shared(cpal::default_host);
    // The server answers, but a host made before it restarted has lost its connection.
    if host.id() == cpal::HostId::PulseAudio && host.devices().is_err() {
        forget(&host);
        return shared(cpal::default_host);
    }
    host
}

/// The host the device lists are read from: the shared one, without asking the server first at
/// every poll. A list that comes back empty lets it go ([`forget`]).
pub(super) fn listing_host() -> Arc<cpal::Host> {
    shared(|| alsa_if_unanswered().unwrap_or_else(cpal::default_host))
}

/// The cpal host every caller shares while it is a Pulse host.
static SHARED: Mutex<Option<Arc<cpal::Host>>> = Mutex::new(None);

/// The shared host, or a new one from `make`, kept if it is a Pulse host. An ALSA host is no
/// connection and is made again at the next call, so a server that comes back is picked up. The
/// lock is held while `make` runs, so two callers never make two.
fn shared(make: impl FnOnce() -> cpal::Host) -> Arc<cpal::Host> {
    let mut shared = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(host) = shared.as_ref() {
        return Arc::clone(host);
    }
    let host = Arc::new(make());
    if host.id() == cpal::HostId::PulseAudio {
        *shared = Some(Arc::clone(&host));
    }
    host
}

/// Lets `host` go if it is the shared one, so the next call makes another.
pub(super) fn forget(host: &Arc<cpal::Host>) {
    let mut shared = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if shared.as_ref().is_some_and(|kept| Arc::ptr_eq(kept, host)) {
        *shared = None;
    }
}

/// cpal's ALSA host when there is a Pulse server and it does not answer within
/// [`PROBE_WITHIN`]; None when it answers, or when there is no ALSA host either.
fn alsa_if_unanswered() -> Option<cpal::Host> {
    pulseaudio::socket_path_from_env()?;
    let Err(error) = Line::connect(Instant::now() + PROBE_WITHIN) else {
        return None;
    };
    warn!(%error, "the sound server does not answer: ALSA is used");
    cpal::host_from_id(cpal::HostId::Alsa)
        .inspect_err(|error| warn!(%error, "no ALSA host either"))
        .ok()
}

/// A connection to the server as Lobby, and a second handle on its socket to cut it with.
struct Line {
    client: Client,
    socket: UnixStream,
}

impl Line {
    /// Connects and does the handshake, both by `deadline`.
    fn connect(deadline: Instant) -> Result<Self, MediaError> {
        let path = pulseaudio::socket_path_from_env()
            .ok_or_else(|| media_error(ClientError::ServerUnavailable))?;
        let cookie = pulseaudio::cookie_path_from_env().and_then(|path| std::fs::read(path).ok());
        let io = |e: std::io::Error| MediaError::Device(e.to_string());
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None).map_err(io)?;
        socket
            .connect_timeout(&SockAddr::unix(&path).map_err(io)?, left(deadline))
            .map_err(io)?;
        let socket = UnixStream::from(socket);
        // The handshake reads and writes this socket blocking: these end it by the deadline.
        socket.set_read_timeout(Some(left(deadline))).map_err(io)?;
        socket.set_write_timeout(Some(left(deadline))).map_err(io)?;
        let cut = socket.try_clone().map_err(io)?;
        let name = CString::new(NAME).expect("no NUL");
        let client = Client::new_unix(name, socket, cookie).map_err(|e| {
            // A read the deadline ended is the crate's protocol error, not a refusal.
            if Instant::now() >= deadline {
                no_answer()
            } else {
                media_error(e)
            }
        })?;
        Ok(Self {
            client,
            socket: cut,
        })
    }
}

impl Drop for Line {
    /// The server sees the client leave and deletes its streams. The client's reactor thread
    /// sees the socket end and ends too, which a server that has stopped answering would never
    /// make it do, and with it every reply still awaited.
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

/// What is left of `deadline`, at least a millisecond: a socket takes no zero timeout.
fn left(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1))
}

struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs `work` on this thread until it is done (`Some`), or `deadline` passes or `stop` is set
/// (`None`). What it waits on is dropped then; the connection's own end is `Line`'s to make.
fn within<T>(work: impl Future<Output = T>, deadline: Instant, stop: &AtomicBool) -> Option<T> {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut work = pin!(work);
    loop {
        if let Poll::Ready(done) = work.as_mut().poll(&mut cx) {
            return Some(done);
        }
        let now = Instant::now();
        if stop.load(Ordering::Acquire) || now >= deadline {
            return None;
        }
        thread::park_timeout(deadline - now);
    }
}

/// Never set: an open is bounded by its deadline alone.
static NEVER: AtomicBool = AtomicBool::new(false);

/// An open stream. Dropping it closes it, within [`CLOSE_WITHIN`] whatever the server does.
pub(super) struct Stream {
    stop: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
    /// Ends when the watcher's thread does.
    watching: mpsc::Receiver<()>,
    /// The server's stream (a playback or a record stream), deleted when it is dropped.
    _stream: Box<dyn Any + Send>,
    /// Dropped last: it cuts the connection.
    _line: Line,
}

/// The question whether the server still has the stream.
type Ask = Box<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send>;
/// A stream the server made, and the question whether it still has it.
type Made = (Box<dyn Any + Send>, Ask);

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let Some(watcher) = self.watcher.take() else {
            return;
        };
        watcher.thread().unpark();
        match self.watching.recv_timeout(CLOSE_WITHIN) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                warn!("a sound stream's watcher did not stop: it is left to end by itself");
            }
            _ => {
                let _ = watcher.join();
            }
        }
    }
}

/// Plays what `render` fills (mono, copied to both channels) on the sink `device`. `lost` is called
/// once, from another thread, if the server stops having the stream.
pub(super) fn output(
    device: String,
    purpose: Purpose,
    mut render: impl FnMut(&mut [f32]) + Send + 'static,
    lost: impl FnOnce() + Send + 'static,
) -> Result<Stream, MediaError> {
    let frame = FRAME * BYTES_PER_SAMPLE * PLAY_CHANNELS;
    let params = PlaybackStreamParams {
        sample_spec: spec(PLAY_CHANNELS),
        channel_map: ChannelMap::stereo(),
        sink_name: Some(cstring(&device)),
        buffer_attr: BufferAttr {
            // Two frames in all and one at a time: with `adjust_latency` this is the whole
            // latency, where a stream that says nothing is given two seconds.
            max_length: bytes(frame * 2),
            target_length: bytes(frame * 2),
            minimum_request_length: bytes(frame),
            ..Default::default()
        },
        flags: flags(),
        props: props(purpose, false),
        ..Default::default()
    };
    let mut scratch = vec![0f32; SCRATCH];
    let source = move |out: &mut [u8]| {
        let whole =
            out.len() / (BYTES_PER_SAMPLE * PLAY_CHANNELS) * BYTES_PER_SAMPLE * PLAY_CHANNELS;
        if whole == 0 {
            // Nothing to write is the end of the stream to the server.
            out.fill(0);
            return out.len();
        }
        for chunk in out[..whole].chunks_mut(SCRATCH * BYTES_PER_SAMPLE * PLAY_CHANNELS) {
            let mono = &mut scratch[..chunk.len() / (BYTES_PER_SAMPLE * PLAY_CHANNELS)];
            render(mono);
            for (frame, sample) in chunk
                .chunks_exact_mut(BYTES_PER_SAMPLE * PLAY_CHANNELS)
                .zip(mono.iter())
            {
                let sample = sample.to_le_bytes();
                frame[..BYTES_PER_SAMPLE].copy_from_slice(&sample);
                frame[BYTES_PER_SAMPLE..].copy_from_slice(&sample);
            }
        }
        whole
    };
    open(
        move |client, deadline| {
            let made = client.create_playback_stream(params, source.as_playback_source());
            let stream = within(made, deadline, &NEVER)
                .ok_or_else(no_answer)?
                .map_err(media_error)?;
            let watched = stream.clone();
            let ask: Ask = Box::new(move || {
                let watched = watched.clone();
                Box::pin(async move { watched.timing_info().await.is_ok() })
            });
            Ok((Box::new(stream), ask))
        },
        lost,
    )
}

/// Records the source `device` as mono samples handed to `on_samples`.
pub(super) fn input(
    device: String,
    purpose: Purpose,
    mut on_samples: impl FnMut(&[f32]) + Send + 'static,
    lost: impl FnOnce() + Send + 'static,
) -> Result<Stream, MediaError> {
    let frame = FRAME * BYTES_PER_SAMPLE;
    let params = RecordStreamParams {
        sample_spec: spec(1),
        channel_map: ChannelMap::mono(),
        source_name: Some(cstring(&device)),
        buffer_attr: BufferAttr {
            max_length: bytes(frame),
            fragment_size: bytes(frame),
            ..Default::default()
        },
        flags: flags(),
        props: props(purpose, true),
        ..Default::default()
    };
    // A write can end inside a sample: the bytes of it that came are kept for the next.
    let mut carry = [0u8; BYTES_PER_SAMPLE];
    let mut carried = 0;
    let mut samples = vec![0f32; SCRATCH];
    let sink = move |data: &[u8]| {
        let mut data = data;
        while !data.is_empty() {
            let mut filled = 0;
            while filled < samples.len() && !data.is_empty() {
                let take = (BYTES_PER_SAMPLE - carried).min(data.len());
                carry[carried..carried + take].copy_from_slice(&data[..take]);
                carried += take;
                data = &data[take..];
                if carried == BYTES_PER_SAMPLE {
                    samples[filled] = f32::from_le_bytes(carry);
                    filled += 1;
                    carried = 0;
                }
            }
            on_samples(&samples[..filled]);
        }
    };
    open(
        move |client, deadline| {
            let made = client.create_record_stream(params, sink);
            let stream = within(made, deadline, &NEVER)
                .ok_or_else(no_answer)?
                .map_err(media_error)?;
            let watched = stream.clone();
            let ask: Ask = Box::new(move || {
                let watched = watched.clone();
                Box::pin(async move { watched.timing_info().await.is_ok() })
            });
            Ok((Box::new(stream), ask))
        },
        lost,
    )
}

fn no_answer() -> MediaError {
    MediaError::Device("the sound server does not answer".into())
}

/// Connects as Lobby and makes the stream, on this thread, within [`DEVICE_START_TIMEOUT`]: a
/// server that stops answering costs that long, and leaves nothing running. Then a thread asks
/// the server every [`WATCH_EVERY`] whether it still has the stream (`ask`) and calls `lost` if
/// not, or if no answer comes within [`ANSWER_WITHIN`].
fn open(
    make: impl FnOnce(&Client, Instant) -> Result<Made, MediaError>,
    lost: impl FnOnce() + Send + 'static,
) -> Result<Stream, MediaError> {
    let deadline = Instant::now() + DEVICE_START_TIMEOUT;
    let line = Line::connect(deadline)?;
    let (held, ask) = make(&line.client, deadline)?;
    let stop = Arc::new(AtomicBool::new(false));
    let (watching, ended) = mpsc::channel::<()>();
    let stopped = Arc::clone(&stop);
    let watcher = thread::Builder::new()
        .name("lobby-pulse-watch".into())
        .spawn(move || {
            let _watching = watching;
            watch(&ask, &stopped, lost);
        })
        .map_err(|e| MediaError::Device(e.to_string()))?;
    Ok(Stream {
        stop,
        watcher: Some(watcher),
        watching: ended,
        _stream: held,
        _line: line,
    })
}

fn watch(ask: &Ask, stop: &AtomicBool, lost: impl FnOnce()) {
    loop {
        // A pause that closing the stream cuts short.
        let _ = within(
            std::future::pending::<()>(),
            Instant::now() + WATCH_EVERY,
            stop,
        );
        if stop.load(Ordering::Acquire) {
            return;
        }
        match within(ask(), Instant::now() + ANSWER_WITHIN, stop) {
            Some(true) => {}
            Some(false) => {
                debug!("the sound server no longer has the stream");
                return lost();
            }
            None if stop.load(Ordering::Acquire) => return,
            None => {
                warn!(within = ?ANSWER_WITHIN, "the sound server stopped answering: stream lost");
                return lost();
            }
        }
    }
}

fn spec(channels: usize) -> SampleSpec {
    SampleSpec {
        format: SampleFormat::Float32Le,
        channels: channels as u8,
        sample_rate: SAMPLE_RATE,
    }
}

/// The server must not move a stream by itself: PipeWire moves one that was opened on a named
/// device when the default changes, which would undo the Member's choice. `devices` decides where
/// a stream is, and reopens it when the default or the list changes.
fn flags() -> StreamFlags {
    StreamFlags {
        adjust_latency: true,
        no_move: true,
        ..Default::default()
    }
}

fn bytes(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn cstring(text: &str) -> CString {
    CString::new(text.replace('\0', "")).expect("no NUL left")
}

/// What the stream says about itself: the app, what it is for and its role.
fn props(purpose: Purpose, input: bool) -> Props {
    let mut props = Props::new();
    let mut set = |prop, value: &str| props.set(prop, cstring(value));
    set(Prop::ApplicationName, NAME);
    set(Prop::ApplicationId, APP_ID);
    set(Prop::ApplicationIconName, APP_ID);
    set(Prop::ApplicationProcessId, &std::process::id().to_string());
    if let Some(binary) = std::env::current_exe()
        .ok()
        .and_then(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
    {
        set(Prop::ApplicationProcessBinary, &binary);
    }
    set(Prop::MediaName, purpose.media_name(input));
    set(Prop::MediaRole, purpose.role());
    props
}

fn media_error(e: ClientError) -> MediaError {
    match e {
        ClientError::ServerError(PulseError::NoEntity) => MediaError::DeviceNotFound,
        ClientError::ServerError(PulseError::AccessDenied | PulseError::AuthKey) => {
            MediaError::PermissionDenied
        }
        ClientError::ServerError(PulseError::Busy | PulseError::Timeout) => MediaError::DeviceBusy,
        other => {
            warn!(error = %other, "the sound server refused");
            MediaError::Device(other.to_string())
        }
    }
}
