use gpui_kit::{Context, InteractiveElement as _, ParentElement as _, Styled as _, Window, div};
use lobby_protocol::{Presence, VoiceState};

use crate::components::{
    MemberRowModel, SURFACE_RADIUS, ShareControl, Tile, VoiceBarModel, VoiceFace, VoiceStrip,
    card_surface,
    voice_bar::{STRIP_HEIGHT, STRIP_PAD},
};
use crate::gallery::{
    Gallery, Section,
    fixtures::{deafened, joining, member, muted, sharing, speaking, voice},
};
use crate::scale::u;

use super::{
    caption, label,
    voice::{BAR_W, in_voice, live, out_of_voice, voice_actions},
    wrap,
};

/// One face in the Voice bar: `tile` for a sharer's.
fn face_row(name: &str, v: VoiceState, tile: Option<Tile>) -> MemberRowModel {
    MemberRowModel {
        voice: Some(v),
        sharing: tile.is_some(),
        tile,
        ..member(name)
    }
}

fn you(row: MemberRowModel) -> MemberRowModel {
    MemberRowModel { is_me: true, ..row }
}

/// A sharer who also has `state` to show: `state` with the share in it.
fn sharing_and(state: VoiceState) -> VoiceState {
    VoiceState {
        screen_share: sharing().screen_share,
        ..state
    }
}

pub(super) fn voice_face(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let surface = card_surface(cx);
    let on_screen = Tile::OnScreen { switching: false };
    let plain = [
        ("In Voice", face_row("alice", voice(), None), true),
        ("You", you(face_row("bob", voice(), None)), true),
        ("Speaking", face_row("carol", speaking(), None), true),
        ("Muted", face_row("dave", muted(), None), true),
        ("Deafened", face_row("Eve Online", deafened(), None), true),
        ("Connecting", face_row("frank", joining(), None), true),
        (
            "Idle",
            MemberRowModel {
                presence: Presence::Idle,
                ..face_row("grace", voice(), None)
            },
            true,
        ),
        (
            "Do Not Disturb",
            MemberRowModel {
                presence: Presence::Dnd,
                ..face_row("heidi", voice(), None)
            },
            true,
        ),
        ("Reconnecting", face_row("ivan", voice(), None), false),
    ];
    let sharers = [
        (
            "At rest",
            face_row("alice", sharing(), Some(Tile::Rest)),
            true,
        ),
        (
            "Coming",
            face_row("bob", sharing(), Some(Tile::Coming)),
            true,
        ),
        (
            "On screen",
            face_row("carol", sharing(), Some(on_screen)),
            true,
        ),
        (
            "On screen, a switch waits",
            face_row("dave", sharing(), Some(Tile::OnScreen { switching: true })),
            true,
        ),
        (
            "Out of Voice",
            face_row("Eve Online", sharing(), Some(Tile::Faint)),
            true,
        ),
        (
            "Speaking",
            face_row("frank", sharing_and(speaking()), Some(Tile::Rest)),
            true,
        ),
        (
            "Muted",
            face_row("grace", sharing_and(muted()), Some(Tile::Rest)),
            true,
        ),
        (
            "Deafened",
            face_row("heidi", sharing_and(deafened()), Some(on_screen)),
            true,
        ),
        (
            "Yours",
            you(face_row("ivan", sharing(), Some(Tile::Rest))),
            true,
        ),
    ];
    let cell = move |(name, row, known): (&'static str, MemberRowModel, bool)| {
        caption(
            name,
            div()
                .h(u(STRIP_HEIGHT))
                .px(u(STRIP_PAD))
                .flex()
                .items_center()
                .rounded(u(SURFACE_RADIUS))
                .bg(surface)
                .child(VoiceFace::new(row, known, surface)),
            cx,
        )
    };
    vec![
        Section::new(
            "plain",
            "A face that shares nothing only names the Member in its tooltip: the presence dot, the \
             speaking ring, a red badge for muted or deafened, and a dim while Voice connects",
            wrap(plain.map(&cell)),
        ),
        Section::new(
            "sharers",
            "A sharer's face wears their watch tile as a badge and is a button that does what the \
             tile does. A mute or deafen badge moves above the screen badge",
            wrap(sharers.map(&cell)),
        ),
    ]
}

pub(super) fn voice_bar(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let surface = card_surface(cx);
    let on_screen = Tile::OnScreen { switching: false };
    let crowd = vec![
        you(face_row("alice", voice(), Some(Tile::Rest))),
        face_row("bob", speaking(), Some(on_screen)),
        face_row("carol", muted(), None),
        face_row("dave", sharing_and(muted()), Some(Tile::Rest)),
        face_row("Eve Online", voice(), None),
        face_row("frank", joining(), None),
        face_row("gus", voice(), None),
        face_row("hana", voice(), None),
    ];
    let names = [
        "alice",
        "bob",
        "carol",
        "dave",
        "Eve Online",
        "frank",
        "gus",
        "hana",
        "ivan",
        "judy",
        "kim",
        "leo",
        "mia",
        "ned",
    ];
    let everyone: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| match i {
            0 => you(face_row(name, voice(), None)),
            1 => face_row(name, speaking(), Some(Tile::Faint)),
            _ => face_row(name, voice(), None),
        })
        .collect();
    let strips = [
        ("Not in Voice, nobody there", out_of_voice(), Vec::new()),
        (
            "Not in Voice, two there: bob's share is faint",
            out_of_voice(),
            vec![
                face_row("bob", sharing(), Some(Tile::Faint)),
                face_row("carol", muted(), None),
            ],
        ),
        (
            "Connecting",
            VoiceBarModel::Connecting,
            vec![you(face_row("alice", joining(), None))],
        ),
        (
            "In Voice, watching bob, who speaks; carol muted, dave a muted sharer",
            in_voice(false, live()),
            crowd[..4].to_vec(),
        ),
        (
            "In Voice, showing your own share",
            in_voice(false, live()),
            vec![
                you(face_row("alice", voice(), Some(on_screen))),
                face_row("bob", voice(), None),
            ],
        ),
        (
            "Eight In Voice: the faces fit",
            in_voice(true, ShareControl::Off),
            crowd,
        ),
        (
            "You are not In Voice, fourteen are: more faces than room, the last one fades out",
            out_of_voice(),
            everyone,
        ),
    ];
    vec![Section::new(
        "bars",
        "The Voice bar above the composer in a window of 480 px: everyone In Voice as faces, you \
         first and then by name, with the controls beside them. Its height never changes",
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(strips.map(|(name, model, rows)| {
                let faces = rows
                    .into_iter()
                    .map(|row| VoiceFace::new(row, true, surface))
                    .collect();
                div()
                    .id(name)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(label(name, cx))
                    .child(
                        div()
                            .w(u(BAR_W))
                            .child(VoiceStrip::new(model, voice_actions()).faces(faces)),
                    )
            })),
    )]
}
