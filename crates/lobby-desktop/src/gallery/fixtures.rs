//! The gallery's plain models and the state its stories keep between frames.

use std::{rc::Rc, sync::Arc, time::Duration};

use gpui_kit::{
    AppContext as _, Context, Entity, ImageSource, RenderImage, ScrollHandle, Window,
    base::{OtpState, input::InputState, slider::SliderState},
};
use image::{Frame, RgbaImage};
use lobby_client::{Issue, IssueKind, ScreenSource, SourceId, TestMotion, VideoFrame};
use lobby_protocol::{
    Attachment, BlobId, MagicCode, Presence, Preview, ScreenShare, ShareId, UserId, VoiceState,
};

use super::Gallery;
use crate::components::{
    CodeTyping, ConnectionNotice, MemberRowModel, PickerModel, PickerView, ShareView,
    connection_notice,
};
use crate::kit::{notification::Notification, select::SelectState};
use lobby_client::Phase;

pub fn user_id(name: &str) -> UserId {
    let hash = crate::components::avatar::stable_hash(name);
    format!("usr_{:024x}", u128::from(hash))
        .parse()
        .expect("a well-formed user id")
}

/// What the connection entry says in a phase, from the same function the app uses. `known` is a
/// Lobby that has been reached since it was opened, so it was last connected at some time.
pub fn notice(phase: Phase, known: bool, last_connected: Option<&str>) -> ConnectionNotice {
    let state = lobby_client::LobbyState {
        phase,
        me: known.then(|| user_id("alice")),
        ..lobby_client::LobbyState::default()
    };
    let last_connected = last_connected.or(known.then_some("21:40"));
    connection_notice(&state, "Dev Lobby", last_connected).expect("a phase that is not calm")
}

pub fn blob(n: u8) -> BlobId {
    format!("blb_{n:024x}")
        .parse()
        .expect("a well-formed blob id")
}

pub fn voice() -> VoiceState {
    VoiceState {
        joining: false,
        muted: false,
        deafened: false,
        speaking: false,
        screen_share: None,
    }
}

pub fn speaking() -> VoiceState {
    VoiceState {
        speaking: true,
        ..voice()
    }
}

pub fn muted() -> VoiceState {
    VoiceState {
        muted: true,
        ..voice()
    }
}

pub fn deafened() -> VoiceState {
    VoiceState {
        muted: true,
        deafened: true,
        ..voice()
    }
}

pub fn joining() -> VoiceState {
    VoiceState {
        joining: true,
        ..voice()
    }
}

pub fn sharing() -> VoiceState {
    VoiceState {
        screen_share: Some(ScreenShare {
            share_id: ShareId::new(1).expect("1 is positive"),
            audio: false,
            source: None,
            drawing: None,
            quality: lobby_protocol::Quality::UpTo2160,
        }),
        ..voice()
    }
}

pub fn member(name: &str) -> MemberRowModel {
    MemberRowModel {
        id: user_id(name),
        name: name.to_owned().into(),
        avatar: None,
        is_me: false,
        presence: Presence::Online,
        voice: None,
        sharing: false,
        tile: None,
        owner: false,
    }
}

pub fn attachment(
    n: u8,
    name: &str,
    mime: &str,
    size: u64,
    preview: Option<(u32, u32)>,
) -> Attachment {
    Attachment {
        id: blob(n),
        name: name.to_owned(),
        mime_type: mime.to_owned(),
        size,
        preview: preview.map(|(width, height)| Preview { width, height }),
    }
}

pub fn picture(w: u32, h: u32) -> Arc<RenderImage> {
    let image = RgbaImage::from_fn(w, h, |x, y| {
        let t = (x + y) as f32 / (w + h) as f32;
        let lerp = |a: f32, b: f32| (a + (b - a) * t) as u8;
        image::Rgba([lerp(220., 160.), lerp(90., 180.), lerp(140., 40.), 255])
    });
    Arc::new(RenderImage::new([Frame::new(image)]))
}

/// A made-up 16:9 screen: a dark desktop with a window and a bar, in the decoder's BGRA.
fn shared_screen() -> VideoFrame {
    let (width, height) = (320_u32, 180_u32);
    let mut bgra = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let in_window = (40..260).contains(&x) && (24..140).contains(&y);
            let title = in_window && y < 38;
            let bar = y >= height - 14;
            let (r, g, b) = if title {
                (70, 90, 160)
            } else if in_window {
                (236, 238, 242)
            } else if bar {
                (30, 34, 48)
            } else {
                (38 + (y / 6) as u8, 52 + (y / 5) as u8, 96 + (x / 8) as u8)
            };
            bgra.extend([b, g, r, 255]);
        }
    }
    VideoFrame {
        width,
        height,
        bgra,
        source: (width, height),
        decoded: 1,
        sharer: None,
    }
}

/// A viewer showing [`shared_screen`], for a story to place and put controls on.
pub fn viewer(name: &str, cx: &mut Context<Gallery>) -> Entity<ShareView> {
    let (_, frames) = tokio::sync::mpsc::channel(1);
    let feed = lobby_client::VideoFeed {
        frames,
        paint: lobby_client::PaintSize::default(),
    };
    cx.new(|cx| {
        let mut view = ShareView::new(name.to_owned(), feed, cx);
        view.push(shared_screen(), std::time::Instant::now());
        view
    })
}

pub fn issue(kind: IssueKind) -> Issue {
    let message = match kind {
        IssueKind::VoiceEnded => "The connection to the Lobby dropped.",
        IssueKind::SendFailed => "Your Message did not go out. It is back in the composer.",
        IssueKind::UploadFailed => "holiday.jpg could not be uploaded.",
        IssueKind::StateWriteFailed => "Not saved on this device.",
        _ => "Something happened that is worth knowing.",
    };
    Issue::new(kind, message)
}

pub fn sources() -> Vec<ScreenSource> {
    let source = |id, name: &str, detail: &str| ScreenSource {
        id,
        name: name.to_owned(),
        detail: detail.to_owned(),
        blocked: false,
    };
    vec![
        source(
            SourceId::Monitor { hmonitor: 1 },
            "Display 1",
            "2560 × 1440 · Primary",
        ),
        source(
            SourceId::Monitor { hmonitor: 2 },
            "Display 2",
            "1920 × 1080",
        ),
        source(
            SourceId::Window { hwnd: 1 },
            "Lobby notes.md - Editor",
            "editor.exe",
        ),
        source(
            SourceId::Window { hwnd: 2 },
            "A very long browser window title that will not fit",
            "browser.exe",
        ),
        source(SourceId::Window { hwnd: 3 }, "Terminal", "terminal"),
        source(SourceId::Window { hwnd: 4 }, "Holiday photos", "photos"),
        ScreenSource {
            blocked: true,
            ..source(SourceId::Window { hwnd: 5 }, "Signal", "Signal")
        },
        source(
            SourceId::TestPattern {
                motion: TestMotion::Thirty,
                size: (1920, 1080),
                tint: None,
            },
            "Test pattern",
            "A moving square at 30 fps",
        ),
    ]
}

/// What the onboarding stories' fields hold, one field per screen drawn.
pub const AUTH_FIELDS: [(&str, &str); 6] = [
    ("lobby.example.com", ""),
    ("lobby.example.com", "localhost:8080"),
    ("you@example.com", "carol@lobby.test"),
    ("you@example.com", "carol@lobby"),
    ("alice", "carol"),
    ("alice", "Carol!"),
];

pub struct Held {
    pub auth_fields: [Entity<InputState>; 6],
    pub codes: [(Entity<OtpState>, Entity<CodeTyping>); 3],
    /// Member volumes at 100 %, 140 % and 0 %, for the cards that show one.
    pub volumes: [Entity<SliderState>; 3],
    /// The name field of the own card while it renames, with a free name and with a taken one.
    pub rename_fields: [Entity<InputState>; 2],
    pub pickers: [Entity<PickerView>; 4],
    /// Watched shares for the viewer stories: a view is drawn in one place at a time.
    pub viewers: [Entity<ShareView>; 11],
    pub toasts: Vec<Entity<Notification>>,
    pub center_scroll: ScrollHandle,
    pub center_heights: crate::components::notification_center::RowHeights,
    pub picture: Arc<RenderImage>,
    /// Who is In Voice on the Member list page's playground.
    pub in_voice: [bool; 4],
    /// The Voice rows' microphone and speakers selects on the sample devices, then on two with
    /// the long names Windows gives.
    pub devices: [Entity<SelectState<crate::voice_settings::Items>>; 4],
    /// Input level at 72 %, and the two Off, Gentle or Strong selects, for the Voice rows: one set
    /// for the steps on and one for the steps off, since a select shows one value at a time.
    pub voice_controls: [crate::voice_settings::VoiceControls; 2],
    pub motion: super::stories::motion::MotionDemo,
}

impl Held {
    pub fn new(window: &mut Window, cx: &mut Context<Gallery>) -> Self {
        let auth_fields = AUTH_FIELDS.map(|(placeholder, value)| {
            let field = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
            field.update(cx, |f, cx| f.set_value(value, window, cx));
            field
        });
        let code = |text: &str, window: &mut Window, cx: &mut Context<Gallery>| {
            let otp = cx.new(|cx| OtpState::new(MagicCode::LEN, window, cx));
            let typing = cx.new(|_| CodeTyping::new(&otp));
            typing.update(cx, |t, cx| t.type_text(text, window, cx));
            (otp, typing)
        };
        let codes = [
            code("", window, cx),
            code("80421579", window, cx),
            code("80421", window, cx),
        ];
        let volumes = [100., 140., 0.].map(|percent| {
            cx.new(|_| {
                SliderState::new()
                    .min(0.)
                    .max(200.)
                    .step(1.)
                    .default_value(percent)
            })
        });
        let rename_fields = ["bobby", "dave"].map(|name| {
            let field = cx.new(|cx| InputState::new(window, cx));
            field.update(cx, |f, cx| f.set_value(name, window, cx));
            field
        });
        // The window with the long title never gives a picture, so it keeps its icon.
        let models = [
            (PickerModel::Loading, false),
            (PickerModel::Sources(sources()), true),
            (PickerModel::Sources(sources()), false),
            (
                PickerModel::Failed(
                    "Screen capture is not available: the graphics driver refused it.".to_owned(),
                ),
                false,
            ),
        ];
        let pickers = models.map(|(model, pictured)| {
            cx.new(|cx| {
                let mut view = PickerView::new(
                    Rc::new(|_, _, _, _| {}),
                    lobby_protocol::Quality::UpTo2160,
                    window,
                    cx,
                );
                view.set_model(model.clone(), cx);
                if let (true, PickerModel::Sources(listed)) = (pictured, &model) {
                    for s in listed
                        .iter()
                        .filter(|s| s.id != SourceId::Window { hwnd: 2 } && !s.blocked)
                    {
                        view.set_thumbnail(&s.id, lobby_client::synthetic_thumbnail(&s.id), cx);
                    }
                }
                view
            })
        });
        let toasts = [
            issue(IssueKind::VoiceEnded),
            issue(IssueKind::SendFailed),
            issue(IssueKind::UploadFailed),
            issue(IssueKind::StateWriteFailed),
        ]
        .iter()
        .map(|i| cx.new(|_| crate::lobby_view::issue_toast(i, gpui_kit::WeakEntity::new_invalid())))
        .collect::<Vec<_>>();
        let connection = [
            notice(
                Phase::Offline {
                    retry_in: Duration::from_secs(4),
                },
                true,
                None,
            ),
            notice(Phase::Unavailable { attempts: 10 }, true, Some("21:40")),
        ];
        let toasts = toasts
            .into_iter()
            .chain(connection.iter().map(|n| {
                cx.new(|_| {
                    crate::lobby_view::connection_toast(n, gpui_kit::WeakEntity::new_invalid())
                })
            }))
            .collect();
        let sample = lobby_client::FakeDevices::sample();
        let long = |name: &str| {
            let device = lobby_client::NamedDevice {
                id: name.into(),
                name: name.into(),
            };
            let endpoints = lobby_client::Endpoints {
                devices: vec![device.clone()],
                default: None,
                communications: None,
            };
            (endpoints, lobby_client::DeviceChoice::Named(device))
        };
        let [long_mic, long_speakers] = [
            long("Microphone (Arctis Nova Pro Wireless Chat Audio Device)"),
            long("Speakers (Realtek(R) High Definition Audio with SST)"),
        ];
        let system = lobby_client::DeviceChoice::SystemDefault;
        let devices = [
            (&sample.inputs, &system),
            (&sample.outputs, &system),
            (&long_mic.0, &long_mic.1),
            (&long_speakers.0, &long_speakers.1),
        ]
        .map(|(endpoints, chosen)| {
            crate::voice_settings::device_select(endpoints, chosen, window, cx)
        });
        Self {
            auth_fields,
            codes,
            devices,
            voice_controls: [(); 2]
                .map(|_| crate::voice_settings::VoiceControls::new(72., window, cx)),
            volumes,
            rename_fields,
            pickers,
            viewers: ["bob"; 11].map(|name| viewer(name, cx)),
            toasts,
            center_scroll: ScrollHandle::new(),
            center_heights: Default::default(),
            picture: picture(160, 160),
            in_voice: [false; 4],
            motion: super::stories::motion::MotionDemo::new(window, cx),
        }
    }

    pub fn image(&self) -> ImageSource {
        ImageSource::Render(self.picture.clone())
    }
}
