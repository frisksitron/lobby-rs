use gpui_kit::{
    AnyElement, App, Context, Div, Hsla, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, assets::IconName, div,
};
use lobby_protocol::{Presence, VoiceState};

use crate::components::{
    Avatar, AvatarSize, AvatarState, IconDisc, MEMBER_ROW_GAP, MemberAvatar, MemberPanel,
    MemberPanelModel, MemberRow, OwnerAction, PanelStep, Rename, Said, Sharer, Tile, WatchLine,
    WatchTile, card_surface, slot_color, voice_accent,
};
use crate::gallery::{
    Gallery, Section,
    fixtures::{deafened, joining, member, muted, sharing, speaking, voice},
};
use crate::kit::ActiveTheme as _;
use crate::scale::u;

use super::{caption, panel, popover_frame, wrap};

/// A column of rows as wide as the narrowest member sheet's list: the 220 sheet less its edge
/// and the list's own padding, with the gap the real list keeps between rows.
fn list(surface: Hsla, cx: &App) -> Div {
    const NARROWEST: f32 = 212.;
    panel(NARROWEST, surface, cx).gap(u(MEMBER_ROW_GAP))
}

pub(super) fn avatar(g: &mut Gallery, _: &mut Window, cx: &mut Context<Gallery>) -> Vec<Section> {
    let sizes = |make: &dyn Fn(AvatarSize) -> Avatar, cx: &App| {
        wrap(AvatarSize::ALL.map(|size| caption(format!("{size:?} {}", size.px()), make(size), cx)))
    };
    let image = g.held.image();
    let names = [
        "alice",
        "bob",
        "carol",
        "dave",
        "Eve Online",
        "frank",
        "grace",
        "heidi",
        "ivan",
        "judy",
        "Mallory Keys",
        "oscar",
    ];
    vec![
        Section::new(
            "sizes",
            "One letter, at every size",
            sizes(&|s| Avatar::member("alice").size(s), cx),
        ),
        Section::new(
            "two-letters",
            "Two letters",
            sizes(&|s| Avatar::member("Mary Jane").size(s), cx),
        ),
        Section::new(
            "lobby-marks",
            "Lobby marks (and the app's own)",
            wrap(
                AvatarSize::ALL
                    .iter()
                    .enumerate()
                    .map(|(i, &s)| {
                        caption(
                            format!("{s:?}"),
                            Avatar::lobby("The Dev Lobby", slot_color(i)).size(s),
                            cx,
                        )
                    })
                    .chain([caption("app", Avatar::app().size(AvatarSize::Hero), cx)]),
            ),
        ),
        Section::new(
            "image",
            "With a picture",
            sizes(
                &|s| Avatar::member("alice").size(s).image(Some(image.clone())),
                cx,
            ),
        ),
        Section::new(
            "identity",
            "Each Member keeps their colour",
            wrap(names.map(|n| caption(n, Avatar::member(n), cx))),
        ),
    ]
}

pub(super) fn state(
    presence: Option<Presence>,
    known: bool,
    voice: Option<VoiceState>,
) -> AvatarState {
    AvatarState {
        presence,
        known,
        voice,
    }
}

pub(super) fn member_avatar(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let presences = [
        ("online", Some(Presence::Online)),
        ("idle", Some(Presence::Idle)),
        ("do not disturb", Some(Presence::Dnd)),
        ("offline", Some(Presence::Offline)),
        ("no badge", None),
    ];
    let row = |known: bool, cx: &App| {
        wrap(presences.iter().map(|(name, p)| {
            caption(
                *name,
                MemberAvatar::new(
                    SharedString::from(format!("p-{name}-{known}")),
                    "alice",
                    state(*p, known, None),
                ),
                cx,
            )
        }))
    };
    let voices = [
        ("In Voice", voice()),
        ("speaking", speaking()),
        ("muted", muted()),
        ("connecting", joining()),
    ];
    let image = g.held.image();
    vec![
        Section::new("presence", "Presence, by shape", row(true, cx)),
        Section::new(
            "reconnecting",
            "Reconnecting: the last known presence, muted",
            row(false, cx),
        ),
        Section::new(
            "voice",
            "Voice",
            wrap(voices.iter().map(|(name, v)| {
                caption(
                    *name,
                    MemberAvatar::new(
                        SharedString::from(format!("v-{name}")),
                        "bob",
                        state(Some(Presence::Online), true, Some(*v)),
                    ),
                    cx,
                )
            })),
        ),
        Section::new(
            "sizes",
            "Speaking, at every size",
            wrap(AvatarSize::ALL.map(|s| {
                caption(
                    format!("{s:?}"),
                    MemberAvatar::new(
                        SharedString::from(format!("s-{s:?}")),
                        "carol",
                        state(Some(Presence::Online), true, Some(speaking())),
                    )
                    .size(s),
                    cx,
                )
            })),
        ),
        Section::new(
            "image",
            "With a picture",
            wrap([
                caption(
                    "online",
                    MemberAvatar::new(
                        "i-online",
                        "alice",
                        state(Some(Presence::Online), true, None),
                    )
                    .image(Some(image.clone())),
                    cx,
                ),
                caption(
                    "speaking",
                    MemberAvatar::new(
                        "i-speaking",
                        "alice",
                        state(Some(Presence::Dnd), true, Some(speaking())),
                    )
                    .image(Some(image)),
                    cx,
                ),
            ]),
        ),
    ]
}

pub(super) fn row(
    model: crate::components::MemberRowModel,
    known: bool,
    in_voice: bool,
    surface: Hsla,
) -> AnyElement {
    MemberRow::new(model, known)
        .in_voice_group(in_voice)
        .surface(surface)
        .into_any_element()
}

pub(super) fn member_row(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let sidebar = cx.theme().sidebar;
    let voice_box = card_surface(cx);
    let me = || crate::components::MemberRowModel {
        is_me: true,
        ..member("bob")
    };
    let everyone = [
        member("alice"),
        me(),
        crate::components::MemberRowModel {
            presence: Presence::Idle,
            ..member("dave")
        },
        crate::components::MemberRowModel {
            presence: Presence::Dnd,
            ..member("Eve Online")
        },
        crate::components::MemberRowModel {
            presence: Presence::Offline,
            ..member("frank")
        },
        member("Maximilian Featherstonehaugh-Wolfeschlegel"),
    ];
    let in_voice = [
        crate::components::MemberRowModel {
            voice: Some(voice()),
            ..member("alice")
        },
        crate::components::MemberRowModel {
            voice: Some(speaking()),
            ..me()
        },
        crate::components::MemberRowModel {
            voice: Some(muted()),
            ..member("carol")
        },
        crate::components::MemberRowModel {
            voice: Some(joining()),
            ..member("dave")
        },
        crate::components::MemberRowModel {
            voice: Some(deafened()),
            ..member("Eve Online")
        },
    ];
    let long = "Maximilian Featherstonehaugh-Wolfeschlegel";
    let columns = [
        sharer("grace", Tile::Rest),
        crate::components::MemberRowModel {
            voice: Some(VoiceState {
                muted: true,
                ..sharing()
            }),
            ..sharer("heidi", Tile::Rest)
        },
        crate::components::MemberRowModel {
            voice: Some(VoiceState {
                deafened: true,
                muted: true,
                ..sharing()
            }),
            ..sharer("ivan", Tile::OnScreen { switching: false })
        },
        crate::components::MemberRowModel {
            voice: Some(muted()),
            ..member("carol")
        },
        crate::components::MemberRowModel {
            voice: Some(VoiceState {
                muted: true,
                ..sharing()
            }),
            ..sharer(long, Tile::Coming)
        },
        crate::components::MemberRowModel {
            voice: Some(speaking()),
            ..sharer("Wilhelmina Featherstonehaugh-Smythe", Tile::Rest)
        },
    ];
    let out_of_voice = [
        sharer("grace", Tile::Faint),
        crate::components::MemberRowModel {
            voice: Some(VoiceState {
                muted: true,
                ..sharing()
            }),
            ..sharer("heidi", Tile::Faint)
        },
    ];
    let reconnecting = [
        member("alice"),
        crate::components::MemberRowModel {
            voice: Some(speaking()),
            ..member("bob")
        },
        crate::components::MemberRowModel {
            presence: Presence::Dnd,
            ..member("carol")
        },
    ];
    vec![
        Section::new(
            "everyone",
            "Normal, you, idle, Do Not Disturb, offline, a long name; in the narrowest sheet",
            list(sidebar, cx).children(everyone.into_iter().map(|m| row(m, true, false, sidebar))),
        ),
        Section::new(
            "in-voice",
            "In the Voice box: In Voice, speaking (you), muted, connecting, deafened",
            list(voice_box, cx)
                .children(in_voice.into_iter().map(|m| row(m, true, true, voice_box))),
        ),
        Section::new(
            "columns",
            "A sharer's row ends in the watch tile, and the muted mark takes a slot of the tile's \
             width before it, so the right edges line up. A muted Member who shares nothing has the \
             mark in the tile's column. A long name gives way to both",
            list(voice_box, cx)
                .children(columns.into_iter().map(|m| row(m, true, true, voice_box))),
        ),
        Section::new(
            "out-of-voice",
            "You are not In Voice: their tiles are faint outlines",
            list(voice_box, cx).children(
                out_of_voice
                    .into_iter()
                    .map(|m| row(m, true, true, voice_box)),
            ),
        ),
        Section::new(
            "reconnecting",
            "Reconnecting: the last known state, muted",
            list(sidebar, cx).children(
                reconnecting
                    .into_iter()
                    .map(|m| row(m, false, true, sidebar)),
            ),
        ),
    ]
}

pub(super) fn panel_model(name: &str) -> MemberPanelModel {
    MemberPanelModel {
        name: name.to_owned().into(),
        is_me: false,
        owner: false,
        watch: WatchLine::Hidden,
        dnd: false,
        in_voice: false,
        manage: false,
        step: PanelStep::Actions,
        since: "12 Mar 2026".into(),
        email: None,
        has_picture: false,
        picture_busy: false,
        rename: Rename::Idle,
    }
}

pub(super) fn member_panel(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let volumes = g.held.volumes.clone();
    let fields = g.held.rename_fields.clone();
    let me = |m: MemberPanelModel| MemberPanelModel {
        is_me: true,
        email: Some("bob@example.com".into()),
        ..m
    };
    let editing = |said: Option<Said>| Rename::Editing {
        can_save: said.is_none(),
        saving: false,
        said,
    };
    let yours = [
        ("Yours", me(panel_model("bob"))),
        (
            "Yours, the owner, with a picture and Do Not Disturb on",
            me(MemberPanelModel {
                owner: true,
                has_picture: true,
                dnd: true,
                ..panel_model("bob")
            }),
        ),
        (
            "Yours, In Voice",
            me(MemberPanelModel {
                in_voice: true,
                ..panel_model("bob")
            }),
        ),
        (
            "Yours, renaming",
            me(MemberPanelModel {
                rename: editing(None),
                ..panel_model("bob")
            }),
        ),
        (
            "Yours, the name is taken",
            me(MemberPanelModel {
                rename: editing(Some(Said {
                    text: lobby_client::copy::NAME_TAKEN.into(),
                    refusal: true,
                })),
                ..panel_model("bob")
            }),
        ),
    ];
    let others = [
        ("Someone", panel_model("alice")),
        (
            "The owner",
            MemberPanelModel {
                owner: true,
                ..panel_model("carol")
            },
        ),
        (
            "Sharing, to watch",
            MemberPanelModel {
                watch: WatchLine::Watch,
                ..panel_model("Eve Online")
            },
        ),
        (
            "Sharing, watched now",
            MemberPanelModel {
                watch: WatchLine::StopWatching,
                ..panel_model("Eve Online")
            },
        ),
        (
            "Sharing, you are not In Voice",
            MemberPanelModel {
                watch: WatchLine::NeedsVoice,
                ..panel_model("Eve Online")
            },
        ),
    ];
    let managed = |step| MemberPanelModel {
        manage: true,
        step,
        ..panel_model("alice")
    };
    let owner = [
        ("Actions", managed(PanelStep::Actions)),
        (
            "Make owner?",
            managed(PanelStep::Confirm(OwnerAction::MakeOwner)),
        ),
        ("Remove?", managed(PanelStep::Confirm(OwnerAction::Remove))),
        ("Removing", managed(PanelStep::Busy(OwnerAction::Remove))),
    ];
    let set = |items: Vec<(&'static str, MemberPanelModel)>, cx: &App| {
        wrap(items.into_iter().enumerate().map(|(i, (name, m))| {
            let panel = MemberPanel::new(m.clone());
            let panel = match &m.rename {
                Rename::Editing { said, .. } => {
                    panel.name_field(fields[usize::from(said.is_some())].clone())
                }
                Rename::Idle if m.is_me => panel,
                Rename::Idle => panel.volume(volumes[i % volumes.len()].clone()),
            };
            caption(name, popover_frame(panel, cx), cx)
        }))
    };
    vec![
        Section::new(
            "yours",
            "Your own row: your name, your email, your picture, and what you can do",
            set(yours.into(), cx),
        ),
        Section::new(
            "someone-else",
            "Someone else's row: their name and since when, then what you can do with them",
            set(others.into(), cx),
        ),
        Section::new(
            "as-owner",
            "As the owner: say what it does, ask once",
            set(owner.into(), cx),
        ),
    ]
}

pub(super) fn icon_disc(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let theme = cx.theme();
    let voice = voice_accent(theme);
    let accents = [
        ("none", None),
        ("Voice", Some(voice)),
        ("success", Some(theme.success)),
        ("info", Some(theme.info)),
        ("warning", Some(theme.warning)),
        ("danger", Some(theme.danger)),
    ];
    let disc = |icon, size, accent: Option<Hsla>| {
        let d = IconDisc::new(icon, size);
        match accent {
            Some(a) => d.accent(a),
            None => d,
        }
    };
    vec![
        Section::new(
            "sizes",
            "Sizes",
            wrap(
                [20., 24., 36., 48.]
                    .map(|s| caption(format!("{s}"), disc(IconName::Bell, s, Some(voice)), cx)),
            ),
        ),
        Section::new(
            "accents",
            "Accents",
            wrap(accents.map(|(name, a)| caption(name, disc(IconName::Wifi, 36., a), cx))),
        ),
        Section::new(
            "hello",
            "Hello: the glyph swells once as the disc appears",
            wrap([caption(
                "hello",
                IconDisc::new(IconName::MessageCircle, 48.).hello("gallery-hello"),
                cx,
            )]),
        ),
    ]
}

fn sharer(name: &str, tile: Tile) -> crate::components::MemberRowModel {
    crate::components::MemberRowModel {
        voice: Some(sharing()),
        sharing: true,
        tile: Some(tile),
        ..member(name)
    }
}

pub(super) fn watch_tile(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let voice_box = card_surface(cx);
    let on_screen = Tile::OnScreen { switching: false };
    let tiles = [
        ("rest", Tile::Rest, false),
        ("rest, hovered", Tile::Rest, true),
        ("coming", Tile::Coming, false),
        ("on screen", on_screen, false),
        ("on screen, hovered", on_screen, true),
        (
            "on screen, a switch waits",
            Tile::OnScreen { switching: true },
            false,
        ),
        ("faint", Tile::Faint, false),
    ];
    let rows = [
        sharer("alice", Tile::OnScreen { switching: true }),
        sharer("bob", Tile::Coming),
        crate::components::MemberRowModel {
            voice: Some(VoiceState {
                muted: true,
                ..sharing()
            }),
            ..sharer("Maximilian Featherstonehaugh-Wolfeschlegel", Tile::Rest)
        },
        crate::components::MemberRowModel {
            voice: Some(muted()),
            ..member("carol")
        },
    ];
    vec![
        Section::new(
            "states",
            "The drawn tile is 44 × 32 in a 44 × 40 slot, the same in every state. Quiet plum at \
             rest; a lavender ring and lift under the pointer; the same lift with a full ring while \
             a switch to it waits for its picture; lavender when it is on screen; a bare outline \
             when you are not In Voice",
            panel(472., voice_box, cx).child(div().flex().flex_wrap().gap_3().p_2().children(
                tiles.into_iter().map(|(name, tile, hovered)| {
                    let watch =
                        WatchTile::new(format!("gallery-tile-{name}"), tile, Sharer::mine());
                    caption(name, if hovered { watch.hovered() } else { watch }, cx)
                }),
            )),
        ),
        Section::new(
            "rows",
            "A switch from alice to bob waits for bob's picture; a muted sharer with a long name, \
             and a muted Member who does not share",
            list(voice_box, cx).children(rows.into_iter().map(|m| row(m, true, true, voice_box))),
        ),
    ]
}
