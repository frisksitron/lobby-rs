//! Snapshots pin the wire bytes: each test encodes a value, compares it with its `insta` snapshot
//! under `tests/snapshots/`, and decodes it back.
//!
//! After an intended wire change, run `INSTA_UPDATE=always cargo test -p lobby-protocol --test
//! golden` (or `cargo insta review`) and review the diff: a changed snapshot is a protocol change.

use std::num::NonZeroU32;

use lobby_protocol::{
    AccessRequest, AccessRequestOutcome, AccessRequestRemoved, AccessToken, Ack, Attachment,
    AuthorRef, ClientFrame, ClientMsg, DrawingEpoch, DrawingSet, ErrorBody, ErrorCode, ErrorDetail,
    ErrorEnvelope, HeldHistory, Hello, Identify, InkBatch, InkPoint, InkRelay, JoinOutcome,
    JoinRequest, JoinStatus, JoinStatusRequest, JoinTicket, LobbyInfo, MagicChallenge, MagicCode,
    MagicCodeRequest, MagicCodeRequested, MagicCodeVerify, Member, MemberRemoved, Message,
    MessageChanges, MessageDelete, MessageEdit, MessageId, MessagePage, MessageRemoved,
    MessageSend, OwnerTransfer, PROTOCOL_VERSION, Presence, PresenceSet, Preview, Quality,
    RawClientFrame, Ready, ReadyHistory, RefreshRequest, RefreshToken, RequestLabel, RtcAnswer,
    RtcOffer, RtcRestart, RtcTrack, ScreenShare, Sdp, ServerMsg, SessionTokens, ShareId,
    ShareSource, ShareStart, ShareWatch, SignOutRequest, SignedIn, SourceSize, StrokeId,
    TrackPurpose, TypingStart, UnixMs, User, UsernameChange, VerifyOutcome, VoiceEndReason,
    VoiceEnded, VoiceFlags, VoiceSessionId, VoiceSpeaking, VoiceState, WaitToken, WatchDropped,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

const SECRET_TOKEN: &str = "q8ZcXUvTq0n3o0b6s1Q2rA";
const SECRET_REFRESH: &str = "r9AdYVwUr1o4p1c7t2R3sB";
/// The ICE password inside the SDP samples: SDP is logged only at trace, never by `Debug`.
const SECRET_ICE_PWD: &str = "Tk7uYb3vXq9pLm2Rz8Wc4Nd6";
const SECRET_TICKET: &str = "v4BeZWxVs2p5q2d8u3S4tC";
const SECRET_CODE: &str = "80421579";
const SECRET_CHALLENGE: &str = "c6DgBZzXu4r7s4f0w5U6vE";
const SECRET_WAIT: &str = "w5CfAXyWt3q6r3e9v4T5uD";
const SECRETS: &[&str] = &[
    SECRET_CHALLENGE,
    SECRET_WAIT,
    SECRET_TOKEN,
    SECRET_REFRESH,
    SECRET_ICE_PWD,
    SECRET_TICKET,
    SECRET_CODE,
];

fn canonical(text: &str) -> Value {
    serde_json::from_str(text).expect("fixture is JSON")
}

fn alice() -> User {
    User {
        id: "usr_9f0e7a9c1b2d3e4f5a6b7c8d".parse().unwrap(),
        username: "alice".parse().unwrap(),
        avatar: None,
        created_at: UnixMs(1_792_000_000_000),
    }
}

fn bob() -> User {
    User {
        id: "usr_0a1b2c3d4e5f60718293a4b5".parse().unwrap(),
        username: "Bob".parse().unwrap(),
        avatar: Some("blb_2582c529769d2a353bd49743".parse().unwrap()),
        created_at: UnixMs(1_792_000_100_000),
    }
}

fn member(user: &User, presence: Presence) -> Member {
    Member {
        id: user.id.clone(),
        username: user.username.clone(),
        avatar: user.avatar.clone(),
        created_at: user.created_at,
        presence,
        voice: None,
    }
}

fn in_voice(mut member: Member, voice: VoiceState) -> Member {
    member.voice = Some(voice);
    member
}

fn vsid() -> VoiceSessionId {
    VoiceSessionId::new(17).unwrap()
}

fn share_id() -> ShareId {
    ShareId::new(3).unwrap()
}

fn source_size() -> SourceSize {
    SourceSize {
        width: NonZeroU32::new(1920).unwrap(),
        height: NonZeroU32::new(1080).unwrap(),
    }
}

fn ink_batch() -> InkBatch {
    InkBatch {
        share_id: share_id(),
        epoch: DrawingEpoch(2),
        stroke: StrokeId(7),
        seq: 4,
        prev: Some(InkPoint { x: 100, y: 200 }),
        points: vec![InkPoint { x: 110, y: 205 }, InkPoint { x: 65535, y: 0 }],
        end: false,
    }
}

fn sdp_sample() -> Sdp {
    Sdp::new(format!(
        "v=0\r\no=str0m-1 1 2 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\na=ice-lite\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=ice-ufrag:Hq5G\r\na=ice-pwd:{SECRET_ICE_PWD}\r\na=mid:0\r\na=sendrecv\r\n"
    ))
}

fn attachment_image_sample() -> Attachment {
    Attachment {
        id: "blb_2582c529769d2a353bd49743".parse().unwrap(),
        name: "cat.png".into(),
        mime_type: "image/png".into(),
        size: 81_234,
        preview: Some(Preview {
            width: 480,
            height: 320,
        }),
    }
}

fn attachment_file_sample() -> Attachment {
    Attachment {
        id: "blb_7d1e4f90a3b2c5d6e8f01a2b".parse().unwrap(),
        name: "notes.pdf".into(),
        mime_type: "application/pdf".into(),
        size: 523_001,
        preview: None,
    }
}

fn message_sample() -> Message {
    Message {
        id: MessageId::new(4182).unwrap(),
        author: AuthorRef {
            id: alice().id,
            username: "alice".into(),
            avatar: None,
            deactivated: false,
        },
        content: "**hi** see `x`".into(),
        attachments: vec![attachment_image_sample(), attachment_file_sample()],
        nonce: "9f2c4a1be07d4c55a1d0f3e2b6a7c8d9".into(),
        created_at: UnixMs(1_792_750_245_123),
        edited_at: None,
        rev: None,
    }
}

fn tokens_sample() -> SessionTokens {
    SessionTokens {
        access_token: AccessToken::new(SECRET_TOKEN),
        access_expires_at: UnixMs(1_792_751_145_123),
        refresh_token: RefreshToken::new(SECRET_REFRESH),
        refresh_expires_at: UnixMs(1_795_342_245_123),
    }
}

fn access_request_sample() -> AccessRequest {
    AccessRequest {
        id: "acr_3c4d5e6f708192a3b4c5d6e7".parse().unwrap(),
        username: "carol".parse().unwrap(),
        requester_label: "carol@example.com".into(),
        created_at: UnixMs(1_792_750_300_000),
    }
}

fn server_sample(kind: &str) -> ServerMsg {
    match kind {
        "HELLO" => ServerMsg::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            server_version: "0.1.0".into(),
            heartbeat_ms: 10_000,
            identify_timeout_ms: 10_000,
        }),
        "READY" => ServerMsg::Ready(Box::new(Ready {
            conn_id: "17".into(),
            me: alice().id,
            email: "alice@example.com".parse().unwrap(),
            lobby: lobby_info_sample(),
            members: vec![
                in_voice(
                    member(&alice(), Presence::Online),
                    VoiceState {
                        joining: false,
                        muted: false,
                        deafened: false,
                        speaking: true,
                        screen_share: Some(ScreenShare {
                            share_id: share_id(),
                            audio: false,
                            source: Some(source_size()),
                            drawing: Some(DrawingEpoch(2)),
                            quality: Quality::UpTo2160,
                        }),
                    },
                ),
                member(&bob(), Presence::Offline),
            ],
            access_requests: vec![access_request_sample()],
            last_message_seq: 4182,
            last_change_rev: 310,
            history: ReadyHistory::After(MessagePage {
                messages: vec![message_sample()],
                has_more: false,
            }),
            history_rev: 309,
        })),
        "ACK" => ServerMsg::Ack(Ack {
            access_expires_at: Some(UnixMs(1_792_751_145_123)),
            ..Ack::new("r7".parse().unwrap())
        }),
        "MEMBER_UPSERT" => ServerMsg::MemberUpsert {
            member: in_voice(
                member(&bob(), Presence::Online),
                VoiceState {
                    joining: true,
                    muted: true,
                    deafened: false,
                    speaking: false,
                    screen_share: None,
                },
            ),
        },
        "MEMBER_REMOVED" => ServerMsg::MemberRemoved(MemberRemoved {
            user_id: alice().id,
        }),
        "VOICE_SPEAKING" => ServerMsg::VoiceSpeaking(VoiceSpeaking {
            user_id: alice().id,
            speaking: true,
        }),
        "RTC_OFFER" => ServerMsg::RtcOffer(RtcOffer {
            voice_session_id: vsid(),
            rev: 2,
            sdp: sdp_sample(),
            tracks: vec![
                RtcTrack {
                    mid: "0".into(),
                    user_id: alice().id,
                    purpose: TrackPurpose::VoiceAudio,
                },
                RtcTrack {
                    mid: "1".into(),
                    user_id: bob().id,
                    purpose: TrackPurpose::VoiceAudio,
                },
                RtcTrack {
                    mid: "2".into(),
                    user_id: alice().id,
                    purpose: TrackPurpose::ScreenVideo,
                },
                RtcTrack {
                    mid: "3".into(),
                    user_id: bob().id,
                    purpose: TrackPurpose::WatchVideo,
                },
            ],
        }),
        "INK" => ServerMsg::Ink(InkRelay {
            author_id: bob().id,
            batch: ink_batch(),
        }),
        "VOICE_ENDED" => ServerMsg::VoiceEnded(VoiceEnded {
            voice_session_id: vsid(),
            reason: VoiceEndReason::ConnectionLost,
        }),
        "WATCH_DROPPED" => ServerMsg::WatchDropped(WatchDropped {
            frames: 58,
            downlink_bps: Some(4_700_000),
        }),
        "MESSAGE_CREATE" => ServerMsg::MessageCreate {
            message: message_sample(),
        },
        "MESSAGE_UPDATE" => ServerMsg::MessageUpdate {
            message: Message {
                content: "**hi** see `y`".into(),
                edited_at: Some(UnixMs(1_792_750_301_456)),
                rev: Some(311),
                ..message_sample()
            },
        },
        "MESSAGE_REMOVED" => ServerMsg::MessageRemoved(MessageRemoved {
            message_id: MessageId::new(4182).unwrap(),
            rev: 312,
        }),
        "ERROR" => ServerMsg::Error(ErrorBody {
            code: ErrorCode::VoiceJoinCooldown,
            message: String::new(),
            detail: None,
            rid: Some("r13".parse().unwrap()),
            retry_after_ms: Some(14_250),
        }),
        "ACCESS_REQUEST_UPSERT" => ServerMsg::AccessRequestUpsert {
            request: access_request_sample(),
        },
        "ACCESS_REQUEST_REMOVED" => ServerMsg::AccessRequestRemoved(AccessRequestRemoved {
            id: access_request_sample().id,
            outcome: AccessRequestOutcome::VouchedIn,
            handled_by: alice().id,
            handled_at: UnixMs(1_792_750_360_000),
        }),
        "TYPING_START" => ServerMsg::TypingStart(TypingStart { user_id: bob().id }),
        "LOBBY_UPDATE" => ServerMsg::LobbyUpdate {
            lobby: LobbyInfo {
                owner: Some(bob().id),
                ..lobby_info_sample()
            },
        },
        other => panic!("no ServerMsg sample for {other}: add one with its fixture"),
    }
}

fn client_sample(kind: &str) -> ClientFrame {
    match kind {
        "IDENTIFY" => ClientFrame::with_rid(
            "r7".parse().unwrap(),
            ClientMsg::Identify(Identify {
                token: AccessToken::new(SECRET_TOKEN),
                presence: Presence::Dnd,
                held: Some(HeldHistory {
                    instance_id: lobby_info_sample().instance_id,
                    newest: MessageId::new(4181).unwrap(),
                }),
            }),
        ),
        "PRESENCE_SET" => ClientFrame::with_rid(
            "p1".parse().unwrap(),
            ClientMsg::PresenceSet(PresenceSet {
                presence: Presence::Idle,
            }),
        ),
        "MESSAGE_SEND" => ClientFrame::with_rid(
            "9f2c4a1be07d4c55a1d0f3e2b6a7c8d9".parse().unwrap(),
            ClientMsg::MessageSend(MessageSend {
                content: "hi <b>".into(),
                attachment_ids: vec![],
            }),
        ),
        "MESSAGE_EDIT" => ClientFrame::with_rid(
            "e1".parse().unwrap(),
            ClientMsg::MessageEdit(MessageEdit {
                message_id: MessageId::new(4182).unwrap(),
                content: "hi, fixed".into(),
            }),
        ),
        "MESSAGE_DELETE" => ClientFrame::with_rid(
            "e2".parse().unwrap(),
            ClientMsg::MessageDelete(MessageDelete {
                message_id: MessageId::new(4182).unwrap(),
            }),
        ),
        "VOICE_JOIN" => ClientFrame::with_rid(
            "v1".parse().unwrap(),
            ClientMsg::VoiceJoin(VoiceFlags {
                muted: false,
                deafened: false,
            }),
        ),
        "VOICE_LEAVE" => ClientFrame::with_rid("r12".parse().unwrap(), ClientMsg::VoiceLeave),
        "VOICE_STATE_SET" => ClientFrame::with_rid(
            "v2".parse().unwrap(),
            ClientMsg::VoiceStateSet(VoiceFlags {
                muted: true,
                deafened: false,
            }),
        ),
        "RTC_ANSWER" => ClientFrame::with_rid(
            "v3".parse().unwrap(),
            ClientMsg::RtcAnswer(RtcAnswer {
                voice_session_id: vsid(),
                rev: 2,
                sdp: sdp_sample(),
            }),
        ),
        "RTC_RESTART" => ClientFrame::with_rid(
            "v4".parse().unwrap(),
            ClientMsg::RtcRestart(RtcRestart {
                voice_session_id: vsid(),
            }),
        ),
        "SCREEN_SHARE_START" => ClientFrame::with_rid(
            "s1".parse().unwrap(),
            ClientMsg::ScreenShareStart(ShareStart {
                audio: false,
                quality: Quality::UpTo1440,
            }),
        ),
        "SCREEN_SHARE_STOP" => {
            ClientFrame::with_rid("s2".parse().unwrap(), ClientMsg::ScreenShareStop)
        }
        "SCREEN_SHARE_WATCH" => ClientFrame::with_rid(
            "s3".parse().unwrap(),
            ClientMsg::ScreenShareWatch(ShareWatch {
                sharer_id: alice().id,
            }),
        ),
        "SCREEN_SHARE_UNWATCH" => {
            ClientFrame::with_rid("s4".parse().unwrap(), ClientMsg::ScreenShareUnwatch)
        }
        "SCREEN_SHARE_SOURCE" => ClientFrame::new(ClientMsg::ScreenShareSource(ShareSource {
            size: source_size(),
        })),
        "DRAWING_SET" => ClientFrame::with_rid(
            "d1".parse().unwrap(),
            ClientMsg::DrawingSet(DrawingSet { enabled: true }),
        ),
        "INK" => ClientFrame::new(ClientMsg::Ink(ink_batch())),
        "TYPING" => ClientFrame::new(ClientMsg::Typing),
        other => panic!("no ClientMsg sample for {other}: add one with its fixture"),
    }
}

fn lobby_info_sample() -> LobbyInfo {
    LobbyInfo {
        instance_id: "lby_5f0e7a9c1b2d3e4f5a6b7c8d".parse().unwrap(),
        name: "Friends".into(),
        icon: Some("blb_2582c529769d2a353bd49743".parse().unwrap()),
        upload_max_bytes: 10_485_760,
        magic_code_sends: 3,
        join_requests_enabled: true,
        set_up: true,
        owner: Some(alice().id),
        protocol_version: PROTOCOL_VERSION,
        server_version: "0.1.0".into(),
    }
}

fn error_envelope_sample() -> ErrorEnvelope {
    ErrorEnvelope {
        error: ErrorBody {
            code: ErrorCode::RateLimited,
            message: "Too many requests".into(),
            detail: None,
            rid: None,
            retry_after_ms: Some(14_250),
        },
    }
}

/// Keys are sorted whatever `serde_json` features other crates in the build turn on.
fn snapshot(name: &str, value: &Value) {
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(name, value);
    });
}

fn check_rest<T>(name: &str, sample: T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(&sample).unwrap();
    snapshot(&format!("rest_{name}"), &encoded);
    let decoded: T = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, sample, "{name}: decoding drifted");
}

#[test]
fn every_server_kind_matches_its_snapshot() {
    for &kind in ServerMsg::ALL_KINDS {
        let sample = server_sample(kind);
        assert_eq!(sample.kind(), kind);
        let text = sample.encode();
        let encoded: Value = canonical(&text);
        assert_eq!(encoded["t"], kind, "serde name and wire_kinds! disagree");
        snapshot(&format!("server_{kind}"), &encoded);
        assert_eq!(
            ServerMsg::decode(&text).unwrap(),
            sample,
            "{kind}: decoding drifted"
        );
    }
}

#[test]
fn every_client_kind_matches_its_snapshot() {
    for &kind in ClientMsg::ALL_KINDS {
        let sample = client_sample(kind);
        assert_eq!(sample.msg.kind(), kind);
        let text = sample.encode();
        let encoded: Value = canonical(&text);
        assert_eq!(encoded["t"], kind, "serde name and wire_kinds! disagree");
        snapshot(&format!("client_{kind}"), &encoded);
        let decoded = RawClientFrame::parse(&text).unwrap().decode().unwrap();
        assert_eq!(decoded, sample, "{kind}: two-stage decoding drifted");
    }
}

fn ticket() -> JoinTicket {
    JoinTicket::new(SECRET_TICKET)
}

fn magic_code_requested_sample() -> MagicCodeRequested {
    MagicCodeRequested {
        challenge: MagicChallenge::new(SECRET_CHALLENGE),
        label: RequestLabel::try_from("KQV".to_owned()).unwrap(),
    }
}

fn magic_code_verify_sample() -> MagicCodeVerify {
    MagicCodeVerify {
        challenge: MagicChallenge::new(SECRET_CHALLENGE),
        code: MagicCode::new(SECRET_CODE).unwrap(),
    }
}

fn verify_ask_to_join_sample() -> VerifyOutcome {
    VerifyOutcome::AskToJoin { ticket: ticket() }
}

fn wait_token() -> WaitToken {
    WaitToken::new(SECRET_WAIT)
}

fn join_sample() -> JoinRequest {
    JoinRequest {
        ticket: ticket(),
        username: "carol".parse().unwrap(),
    }
}

fn signed_in_sample() -> SignedIn {
    SignedIn {
        user: alice(),
        tokens: tokens_sample(),
    }
}

fn refresh_sample() -> RefreshRequest {
    RefreshRequest {
        refresh_token: RefreshToken::new(SECRET_REFRESH),
    }
}

fn message_page_sample() -> MessagePage {
    MessagePage {
        messages: vec![message_sample()],
        has_more: true,
    }
}

#[test]
fn rest_bodies_match_their_snapshots() {
    check_rest("LobbyInfo", lobby_info_sample());
    check_rest("Attachment", attachment_image_sample());
    check_rest("ErrorEnvelope", error_envelope_sample());
    check_rest("RefreshRequest", refresh_sample());
    check_rest("SessionTokens", tokens_sample());
    check_rest("SignedIn", signed_in_sample());
    check_rest("MessagePage", message_page_sample());
    check_rest(
        "MessageChanges",
        MessageChanges {
            updated: vec![Message {
                edited_at: Some(UnixMs(1_792_750_301_456)),
                rev: Some(311),
                ..message_sample()
            }],
            removed: vec![MessageId::new(4183).unwrap()],
        },
    );
    check_rest("OwnerTransfer", OwnerTransfer { user_id: bob().id });
    check_rest(
        "ScreenShare-minimal",
        ScreenShare {
            share_id: share_id(),
            audio: true,
            source: None,
            drawing: None,
            quality: Quality::UpTo1080,
        },
    );
    check_rest(
        "Quality",
        [Quality::UpTo1080, Quality::UpTo1440, Quality::UpTo2160],
    );
    check_rest(
        "InkBatch-pen_up",
        InkBatch {
            prev: None,
            points: vec![],
            end: true,
            ..ink_batch()
        },
    );
    check_rest(
        "UsernameChange",
        UsernameChange {
            username: "alice_2".parse().unwrap(),
        },
    );
    check_rest(
        "ErrorEnvelope-conflict",
        ErrorEnvelope {
            error: ErrorBody {
                code: ErrorCode::Conflict,
                message: "Username already taken".into(),
                detail: Some(ErrorDetail::UsernameTaken),
                rid: None,
                retry_after_ms: None,
            },
        },
    );
    check_rest(
        "MagicCodeRequest",
        MagicCodeRequest {
            email: "carol@example.com".parse().unwrap(),
        },
    );
    check_rest("MagicCodeRequested", magic_code_requested_sample());
    check_rest("MagicCodeVerify", magic_code_verify_sample());
    check_rest(
        "VerifyOutcome-session",
        VerifyOutcome::Session {
            session: signed_in_sample(),
        },
    );
    check_rest(
        "VerifyOutcome-pending_approval",
        VerifyOutcome::PendingApproval {
            wait_token: wait_token(),
        },
    );
    check_rest(
        "VerifyOutcome-create_lobby",
        VerifyOutcome::CreateLobby { ticket: ticket() },
    );
    check_rest("VerifyOutcome-ask_to_join", verify_ask_to_join_sample());
    check_rest("JoinRequest", join_sample());
    check_rest(
        "JoinOutcome-session",
        JoinOutcome::Session {
            session: signed_in_sample(),
        },
    );
    check_rest(
        "JoinOutcome-pending_approval",
        JoinOutcome::PendingApproval {
            wait_token: wait_token(),
        },
    );
    check_rest(
        "JoinStatusRequest",
        JoinStatusRequest {
            wait_token: wait_token(),
        },
    );
    check_rest("JoinStatus-pending_approval", JoinStatus::PendingApproval);
    check_rest(
        "JoinStatus-session",
        JoinStatus::Session {
            session: signed_in_sample(),
        },
    );
    check_rest("JoinStatus-declined", JoinStatus::Declined);
    check_rest("SignOutRequest", SignOutRequest { everywhere: true });
}

fn assert_no_secret(what: &str, debug: &str) {
    for secret in SECRETS {
        assert!(!debug.contains(secret), "{what} leaks a secret: {debug}");
    }
}

#[test]
fn debug_output_of_samples_contains_no_secret() {
    for &kind in ClientMsg::ALL_KINDS {
        assert_no_secret(kind, &format!("{:?}", client_sample(kind)));
    }
    for &kind in ServerMsg::ALL_KINDS {
        assert_no_secret(kind, &format!("{:?}", server_sample(kind)));
    }
    assert_no_secret("RefreshRequest", &format!("{:?}", refresh_sample()));
    assert_no_secret("SessionTokens", &format!("{:?}", tokens_sample()));
    assert_no_secret("SignedIn", &format!("{:?}", signed_in_sample()));
    assert_no_secret(
        "MagicCodeRequested",
        &format!("{:?}", magic_code_requested_sample()),
    );
    assert_no_secret(
        "MagicCodeVerify",
        &format!("{:?}", magic_code_verify_sample()),
    );
    assert_no_secret(
        "VerifyOutcome",
        &format!("{:?}", verify_ask_to_join_sample()),
    );
    assert_no_secret("JoinRequest", &format!("{:?}", join_sample()));
    assert_no_secret(
        "JoinOutcome",
        &format!(
            "{:?}",
            JoinOutcome::PendingApproval {
                wait_token: wait_token()
            }
        ),
    );
}
