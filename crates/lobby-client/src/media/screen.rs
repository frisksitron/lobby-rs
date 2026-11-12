use super::{
    MediaError,
    pattern::{DEFAULT_SIZE, SMALLEST},
};

#[cfg(all(windows, feature = "test-support"))]
pub use platform::{grab_monitor, grab_window};
#[cfg(windows)]
pub(super) use platform::{monitor, start_capture};

/// A rectangle of a picture: x, y, width and height.
pub(crate) type Rect = (u32, u32, u32, u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Screen,
    Window,
    TestPattern,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceId {
    /// The `HMONITOR` Windows listed it by. The capture and the Sharer's ink overlay both use it
    /// as is, so the overlay covers exactly the monitor that is captured.
    Monitor {
        hmonitor: isize,
    },
    Window {
        hwnd: isize,
    },
    TestPattern {
        motion: TestMotion,
        size: (u32, u32),
        /// One colour, as B, G, R, in place of the gradient: tests tell two Sharers apart by it.
        tint: Option<[u8; 3]>,
    },
    /// Fake media's window that keeps itself out of captures, as Signal's does.
    TestBlocked,
}

/// The test pattern still, with its square moving at 30 or 60 pictures a second, or as a screen
/// of text scrolling at 60 that changes every pixel of the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestMotion {
    Still,
    Thirty,
    Sixty,
    Scroll,
    /// The scroll with a quarter of the picture a moving texture, as a video playing.
    ScrollTexture,
    /// The scroll with a quarter of the picture fresh noise every picture: the worst case.
    ScrollNoise,
}

impl SourceId {
    /// The source keeps itself out of captures, so a share of it would never show a picture.
    pub(super) fn blocks_capture(&self) -> bool {
        match self {
            #[cfg(windows)]
            SourceId::Window { hwnd } => super::win::blocks_capture(*hwnd as *mut std::ffi::c_void),
            SourceId::TestBlocked => true,
            _ => false,
        }
    }

    /// A game in exclusive fullscreen is this window, or is on this monitor.
    #[cfg(windows)]
    pub(super) fn under_exclusive_fullscreen(&self) -> bool {
        let Some((window, monitor)) = super::win::exclusive_fullscreen() else {
            return false;
        };
        match self {
            SourceId::Window { hwnd } => *hwnd == window,
            SourceId::Monitor { hmonitor } => *hmonitor == monitor,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScreenSource {
    pub id: SourceId,
    pub name: String,
    pub detail: String,
    /// It kept itself out of captures when it was listed: the picker shows it and cannot pick
    /// it. A share checks again, since an app can turn that on at any time.
    pub blocked: bool,
}

impl ScreenSource {
    pub fn kind(&self) -> SourceKind {
        match self.id {
            SourceId::Monitor { .. } => SourceKind::Screen,
            SourceId::Window { .. } => SourceKind::Window,
            SourceId::TestPattern { .. } | SourceId::TestBlocked => SourceKind::TestPattern,
        }
    }

    pub fn test_pattern(motion: TestMotion) -> ScreenSource {
        Self::test_pattern_sized(motion, DEFAULT_SIZE)
    }

    /// The test pattern at another picture size, at least 640 × 360.
    pub fn test_pattern_sized(motion: TestMotion, size: (u32, u32)) -> ScreenSource {
        assert!(
            size.0 >= SMALLEST.0
                && size.1 >= SMALLEST.1
                && size.0.is_multiple_of(2)
                && size.1.is_multiple_of(2),
            "a test pattern of {size:?}"
        );
        ScreenSource {
            id: SourceId::TestPattern {
                motion,
                size,
                tint: None,
            },
            name: match motion {
                TestMotion::Still => "Test pattern (still)",
                TestMotion::Thirty => "Test pattern",
                TestMotion::Sixty => "Test pattern (60 fps)",
                TestMotion::Scroll => "Test pattern (scrolling text)",
                TestMotion::ScrollTexture => "Test pattern (scrolling text, moving texture)",
                TestMotion::ScrollNoise => "Test pattern (scrolling text, fresh noise)",
            }
            .into(),
            detail: format!("{} × {} synthetic picture", size.0, size.1),
            blocked: false,
        }
    }

    /// The test pattern in one colour, `bgr`, with its square still moving.
    pub fn test_pattern_tinted(motion: TestMotion, bgr: [u8; 3]) -> ScreenSource {
        let mut source = Self::test_pattern(motion);
        source.id = SourceId::TestPattern {
            motion,
            size: DEFAULT_SIZE,
            tint: Some(bgr),
        };
        source
    }

    pub fn test_blocked() -> ScreenSource {
        ScreenSource {
            id: SourceId::TestBlocked,
            name: "Private test window".into(),
            detail: "messenger".into(),
            blocked: true,
        }
    }
}

/// On Windows the monitors, then the on-screen windows of other processes, by name (tool windows
/// and DWM-cloaked ones left out, those that block capture marked). Plus the test patterns and a
/// window that blocks capture when asked for.
/// Enumeration takes a few milliseconds; callers keep it off the UI thread.
pub fn list_sources(test_patterns: bool) -> Result<Vec<ScreenSource>, MediaError> {
    let mut sources = platform::sources()?;
    if test_patterns {
        for motion in [TestMotion::Thirty, TestMotion::Sixty, TestMotion::Still] {
            sources.push(ScreenSource::test_pattern(motion));
        }
        let mut big = ScreenSource::test_pattern_sized(TestMotion::Thirty, (3840, 2160));
        big.name = "Test pattern (4K)".into();
        sources.push(big);
        sources.push(ScreenSource::test_blocked());
    }
    Ok(sources)
}

#[cfg(windows)]
mod platform {
    use std::{ffi::c_void, fmt::Display};

    use windows_capture::{
        capture::{CaptureControl, GraphicsCaptureApiError, GraphicsCaptureApiHandler},
        monitor::Monitor,
        settings::{
            ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
            GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings,
            Settings,
        },
        window::Window,
    };

    use super::{ScreenSource, SourceId};
    use crate::media::MediaError;

    pub fn sources() -> Result<Vec<ScreenSource>, MediaError> {
        let monitors =
            Monitor::enumerate().map_err(|e| MediaError::Capture(format!("monitors: {e}")))?;
        let primary = Monitor::primary().ok().map(|m| m.as_raw_hmonitor());
        let mut sources: Vec<ScreenSource> = monitors
            .iter()
            .filter_map(|m| {
                let index = m.index().ok()?;
                let size = format!("{} × {}", m.width().ok()?, m.height().ok()?);
                let is_primary = primary == Some(m.as_raw_hmonitor());
                let label = m.name().ok().filter(|n| !n.trim().is_empty());
                Some(ScreenSource {
                    id: SourceId::Monitor {
                        hmonitor: m.as_raw_hmonitor() as isize,
                    },
                    name: match label {
                        Some(label) => format!("Display {index} · {label}"),
                        None => format!("Display {index}"),
                    },
                    detail: if is_primary {
                        format!("{size} · Primary")
                    } else {
                        size
                    },
                    blocked: false,
                })
            })
            .collect();
        let me = std::process::id();
        let windows =
            Window::enumerate().map_err(|e| MediaError::Capture(format!("windows: {e}")))?;
        let mut listed: Vec<ScreenSource> = windows
            .iter()
            .filter(|w| w.process_id().ok() != Some(me))
            .filter(|w| !crate::media::win::is_cloaked(w.as_raw_hwnd()))
            .filter_map(|w| {
                let title = w.title().ok().filter(|t| !t.trim().is_empty())?;
                let process = w.process_name().unwrap_or_default();
                let id = SourceId::Window {
                    hwnd: w.as_raw_hwnd() as isize,
                };
                Some(ScreenSource {
                    blocked: id.blocks_capture(),
                    id,
                    name: title,
                    detail: process.trim_end_matches(".exe").to_owned(),
                })
            })
            .collect();
        listed.sort_by_key(|s| s.name.to_lowercase());
        sources.extend(listed);
        Ok(sources)
    }

    /// One whole picture of a monitor as a share captures it, in BGRA rows without padding and
    /// without the pointer, for checks of what a share of the screen would show.
    #[cfg(feature = "test-support")]
    pub fn grab_monitor(
        hmonitor: isize,
        timeout: std::time::Duration,
    ) -> Result<(u32, u32, Vec<u8>), MediaError> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let control = start_capture::<WholeFrame, Monitor>(
            monitor(hmonitor)?,
            CursorCaptureSettings::WithoutCursor,
            MinimumUpdateIntervalSettings::Default,
            tx,
        )
        .map_err(MediaError::Capture)?;
        let got = rx.recv_timeout(timeout);
        let _ = control.stop();
        got.map_err(|_| MediaError::Capture("no picture in time".into()))
    }

    /// One whole picture of a window as a share captures it, like [`grab_monitor`].
    #[cfg(feature = "test-support")]
    pub fn grab_window(
        hwnd: isize,
        timeout: std::time::Duration,
    ) -> Result<(u32, u32, Vec<u8>), MediaError> {
        let window = Window::from_raw_hwnd(hwnd as *mut c_void);
        if !window.is_valid() {
            return Err(MediaError::SourceGone);
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let control = start_capture::<WholeFrame, Window>(
            window,
            CursorCaptureSettings::WithoutCursor,
            MinimumUpdateIntervalSettings::Default,
            tx,
        )
        .map_err(MediaError::Capture)?;
        let got = rx.recv_timeout(timeout);
        let _ = control.stop();
        got.map_err(|_| MediaError::Capture("no picture in time".into()))
    }

    #[cfg(feature = "test-support")]
    struct WholeFrame(Option<std::sync::mpsc::SyncSender<(u32, u32, Vec<u8>)>>);

    #[cfg(feature = "test-support")]
    impl GraphicsCaptureApiHandler for WholeFrame {
        type Flags = std::sync::mpsc::SyncSender<(u32, u32, Vec<u8>)>;
        type Error = MediaError;

        fn new(ctx: windows_capture::capture::Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(WholeFrame(Some(ctx.flags)))
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut windows_capture::frame::Frame,
            control: windows_capture::graphics_capture_api::InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            if let Some(sent) = self.0.take() {
                let (width, height) = (frame.width(), frame.height());
                let buffer = frame
                    .buffer()
                    .map_err(|e| MediaError::Capture(format!("frame readback: {e}")))?;
                let mut rows = Vec::new();
                let bgra = buffer.as_nopadding_buffer(&mut rows).to_vec();
                let _ = sent.send((width, height, bgra));
            }
            control.stop();
            Ok(())
        }
    }

    /// The monitor behind a listed `HMONITOR`, while Windows still knows it: a display change
    /// since the listing makes the handle stale.
    pub fn monitor(hmonitor: isize) -> Result<Monitor, MediaError> {
        let monitor = Monitor::from_raw_hmonitor(hmonitor as *mut c_void);
        monitor
            .device_name()
            .map(|_| monitor)
            .map_err(|_| MediaError::SourceGone)
    }

    /// Captures `item` in BGRA on a thread of its own; the error is the last attempt's.
    pub fn start_capture<H, T>(
        item: T,
        cursor: CursorCaptureSettings,
        interval: MinimumUpdateIntervalSettings,
        flags: H::Flags,
    ) -> Result<CaptureControl<H, H::Error>, String>
    where
        H: GraphicsCaptureApiHandler + Send + 'static,
        H::Flags: Clone + Send,
        GraphicsCaptureApiError<H::Error>: Display,
        T: TryInto<GraphicsCaptureItemType> + Send + Clone + 'static,
    {
        // The yellow border is off where Windows allows it (Windows 11); older systems refuse
        // the setting, so the capture is retried with the default.
        let attempt = |border| {
            let settings = Settings::new(
                item.clone(),
                cursor,
                border,
                SecondaryWindowSettings::Default,
                interval,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                flags.clone(),
            );
            H::start_free_threaded(settings)
        };
        attempt(DrawBorderSettings::WithoutBorder)
            .or_else(|_| attempt(DrawBorderSettings::Default))
            .map_err(|e| e.to_string())
    }
}
