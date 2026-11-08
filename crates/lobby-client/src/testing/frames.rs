use lobby_protocol::{
    Email, Member, Message, MessagePage, PROTOCOL_VERSION, Presence, Ready, ReadyHistory,
    ServerMsg, UnixMs, UserId, VoiceEnded,
};

use super::FakeLobby;

fn user_id(name: &str) -> UserId {
    let mut hex = String::new();
    for b in name.bytes().chain(std::iter::repeat(b'0')).take(12) {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("usr_{hex}").parse().expect("valid id")
}

pub fn member(name: &str, presence: Presence) -> Member {
    Member {
        id: user_id(name),
        username: name.parse().expect("valid username"),
        avatar: None,
        created_at: UnixMs(1_792_000_000_000),
        presence,
        voice: None,
    }
}

pub fn in_voice(name: &str, joining: bool, muted: bool, speaking: bool) -> ServerMsg {
    let mut m = member(name, Presence::Online);
    m.voice = Some(lobby_protocol::VoiceState {
        joining,
        muted,
        deafened: false,
        speaking,
        screen_share: None,
    });
    ServerMsg::MemberUpsert { member: m }
}

pub fn sharing(name: &str) -> ServerMsg {
    sharing_ink(name, 1, None)
}

/// `name` streams share `share` with "Let viewers draw" at epoch `drawing` (off when `None`).
pub fn sharing_ink(name: &str, share: u64, drawing: Option<u32>) -> ServerMsg {
    let mut m = member(name, Presence::Online);
    m.voice = Some(lobby_protocol::VoiceState {
        joining: false,
        muted: false,
        deafened: false,
        speaking: false,
        screen_share: Some(lobby_protocol::ScreenShare {
            share_id: lobby_protocol::ShareId::new(share).expect("share ids are positive"),
            audio: false,
            source: None,
            drawing: drawing.map(lobby_protocol::DrawingEpoch),
            quality: lobby_protocol::Quality::UpTo2160,
        }),
    });
    ServerMsg::MemberUpsert { member: m }
}

pub fn not_in_voice(name: &str) -> ServerMsg {
    ServerMsg::MemberUpsert {
        member: member(name, Presence::Online),
    }
}

pub fn ack(rid: &str) -> ServerMsg {
    ServerMsg::Ack(lobby_protocol::Ack::new(rid.parse().expect("valid rid")))
}

pub fn voice_joined(rid: &str, vsid: u64) -> ServerMsg {
    ServerMsg::Ack(lobby_protocol::Ack {
        voice_session_id: Some(lobby_protocol::VoiceSessionId::new(vsid).expect("vsid > 0")),
        ..lobby_protocol::Ack::new(rid.parse().expect("valid rid"))
    })
}

pub fn speaking(name: &str, speaking: bool) -> ServerMsg {
    ServerMsg::VoiceSpeaking(lobby_protocol::VoiceSpeaking {
        user_id: user_id(name),
        speaking,
    })
}

pub fn voice_ended(vsid: u64, reason: lobby_protocol::VoiceEndReason) -> ServerMsg {
    ServerMsg::VoiceEnded(VoiceEnded {
        voice_session_id: lobby_protocol::VoiceSessionId::new(vsid).expect("vsid > 0"),
        reason,
    })
}

pub fn ready(me: &str, others: &[(&str, Presence)]) -> ServerMsg {
    let email = Email::parse(&format!("{}@example.com", me.to_lowercase())).expect("valid email");
    let me = member(me, Presence::Online);
    let mut members: Vec<Member> = others.iter().map(|(n, p)| member(n, *p)).collect();
    members.push(me.clone());
    ServerMsg::Ready(Box::new(Ready {
        conn_id: "1".into(),
        me: me.id.clone(),
        email,
        lobby: FakeLobby::lobby_info(PROTOCOL_VERSION),
        members,
        access_requests: vec![],
        last_message_seq: 0,
        last_change_rev: 0,
        history: ReadyHistory::Latest(MessagePage {
            messages: vec![],
            has_more: false,
        }),
        history_rev: 0,
    }))
}

pub fn access_request(name: &str, minute: i64) -> lobby_protocol::AccessRequest {
    lobby_protocol::AccessRequest {
        id: request_id(name),
        username: name.parse().expect("valid username"),
        requester_label: format!("{name}@example.com"),
        created_at: UnixMs(1_792_000_000_000 + minute * 60_000),
    }
}

pub fn request_id(name: &str) -> lobby_protocol::AccessRequestId {
    let mut hex: String = name
        .bytes()
        .flat_map(|b| [b >> 4, b & 0xf])
        .map(|n| char::from_digit(u32::from(n), 16).expect("hex digit"))
        .collect();
    hex.truncate(24);
    format!("acr_{hex:0<24}").parse().expect("valid id")
}

pub fn message(id: i64, author: &str, content: &str, nonce: &str) -> ServerMsg {
    ServerMsg::MessageCreate {
        message: Message {
            id: lobby_protocol::MessageId::new(id).expect("positive id"),
            author: lobby_protocol::AuthorRef {
                id: user_id(author),
                username: author.into(),
                avatar: None,
                deactivated: false,
            },
            content: content.into(),
            attachments: Vec::new(),
            nonce: nonce.into(),
            created_at: UnixMs(1_792_750_245_123),
            edited_at: None,
            rev: None,
        },
    }
}
