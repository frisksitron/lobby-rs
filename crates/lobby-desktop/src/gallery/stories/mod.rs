mod auth;
mod feed;
mod floating_sheet;
mod member_list;
mod members;
pub(super) mod motion;
mod notifications;
mod title_bar;
mod viewer;
mod voice;
mod voice_bar;
mod voice_settings;

use std::cell::Cell;

use gpui_kit::{
    AnyElement, App, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div, px,
};

use super::{Story, controls};
use crate::kit::ActiveTheme as _;

pub const STORIES: &[Story] = &[
    Story {
        name: "Avatar",
        description: "A Member's face or a Lobby's mark: one initials rule, centred at every size; a picture takes the initials' place.",
        usage: "Avatar::member(name).size(size).image(image)\nAvatar::lobby(name, color).size(AvatarSize::Bar)",
        render: members::avatar,
    },
    Story {
        name: "MemberAvatar",
        description: "The avatar with presence (by shape), the speaking ring, and dimming while Voice connects or the Member is offline.",
        usage: "MemberAvatar::new(format!(\"avatar-{key}\"), name, state).surface(surface)",
        render: members::member_avatar,
    },
    Story {
        name: "BarButton",
        description: "The title bar's controls: one height, radius, icon size and hover, each occluding the window drag, with an optional dot.",
        usage: "ui::Button::new(\"notification-bell\").size(ui::Size::Bar).icon(IconName::Bell)\nBarButton::slot(popover, Some(color))",
        render: title_bar::bar_button,
    },
    Story {
        name: "IconDisc",
        description: "A glyph on a soft disc: a notification's source, a connection state, an auth screen's status, an empty state.",
        usage: "IconDisc::new(IconName::UserPlus, 48.).accent(voice_accent(cx.theme()))\nIconDisc::new(IconName::MessageCircle, 48.).hello(\"feed-empty-hello\")",
        render: members::icon_disc,
    },
    Story {
        name: "MemberRow",
        description: "One Member in the member list, yourself included: one height, presence on the avatar, then the muted mark and a sharer's watch tile in columns at the end. A right-click opens their Member Card at the pointer.",
        usage: "MemberRow::new(row, known)\n    .in_voice_group(in_voice_group)\n    .surface(surface)\n    .open(card_is_open)\n    .actions(actions)",
        render: members::member_row,
    },
    Story {
        name: "Watch tile",
        description: "The screen-shaped tile at a sharer's row's end, the same size in every state: quiet plum at rest, a lavender ring on hover and while a switch to it waits, lavender for the share on screen, an outline when you are not In Voice.",
        usage: "WatchTile::new(format!(\"watch-{name}\"), tile, Sharer::of(&row)).on_press(on_press)",
        render: members::watch_tile,
    },
    Story {
        name: "Member list",
        description: "The member list: no headings. Who is In Voice sits in a purple box at the top, then everyone online, then offline. Rows keep one gap, inside the box too, so joining or leaving grows or shrinks the box and moves no other row.",
        usage: "member_list(&groups, voice_surface, before_ready, cx)",
        render: member_list::member_list,
    },
    Story {
        name: "Floating sheet",
        description: "The one surface lifted off the window, by its edge alone: the member list with the Voice foot at its foot, docked beside the chat and folded over the dimmed small window.",
        usage: "floating_sheet(cx).w(u(width)).flex().flex_col().child(list).child(foot)",
        render: floating_sheet::floating_sheet_story,
    },
    Story {
        name: "MemberPanel",
        description: "The Member Card a right-click on a Member's row or face opens, text only: their name, the Owner pill and, for someone else, since when they have been a Member. Yours has a pencil that turns the name into a field with Save, your email, your picture, Do Not Disturb, Voice stats while In Voice, and Sign out. Someone else's has Watch while they share and a volume slider with a reset, and for the owner Make owner and Remove, each asked once more.",
        usage: "MemberPanel::new(model).volume(slider).name_field(input).actions(PanelActions { reset_volume, set_dnd, voice_stats, sign_out, ask, confirm, start_rename, save_rename, change_picture, remove_picture })",
        render: members::member_panel,
    },
    Story {
        name: "Voice foot",
        description: "Mute, deafen and Share Screen as one button group with Leave beside it, or Join Voice in their place, the same height in every state. At the member sheet's foot, and at the small window's Voice bar.",
        usage: "VoiceFoot::new(voice_bar_model(state), actions).waits(Waits::of(state, now)).size(FootSize::Sheet | FootSize::Bar)",
        render: voice::voice_foot,
    },
    Story {
        name: "Join to Leave",
        description: "The starting point for the animation: the foot Out of Voice, while Voice connects and In Voice, in the member sheet at both its widths and in the Voice bar. Join Voice turns into the group and Leave, which stands at its right end.",
        usage: "VoiceFoot::new(VoiceBarModel::Idle { can_join }, actions)\nVoiceFoot::new(VoiceBarModel::Connecting, actions)\nVoiceFoot::new(VoiceBarModel::InVoice { .. }, actions)",
        render: voice::join_to_leave,
    },
    Story {
        name: "Voice face",
        description: "One Member's face in the small window's Voice bar: presence, the speaking ring, a red badge for muted or deafened, and for a sharer their watch tile as a badge, which is the button that watches.",
        usage: "VoiceFace::new(row, known, surface).pictures(pictures).on_press(press)",
        render: voice_bar::voice_face,
    },
    Story {
        name: "Voice bar",
        description: "The small window's Voice bar above the composer: a face for everyone In Voice, you first and then by name, with the foot at the bar's size. 44 px tall in every state, with nobody there, while connecting, and with more faces than room.",
        usage: "VoiceStrip::new(voice_bar_model(state), actions).waits(Waits::of(state, now)).faces(faces)",
        render: voice_bar::voice_bar,
    },
    Story {
        name: "Voice settings",
        description: "Settings' Voice section in three groups under small headings. Microphone: its select; the twenty-segment level meter, lavender and coral where it nears clipping, beside the Hear yourself button; a line kept for what is wrong (the microphone failed, too quiet, too loud, clipping, or the reminder to use headphones); and the volume in Windows, a slider with its percent (the reason replaces it when Windows cannot change it). Speakers: its select. Sound: the three clean-up steps (cancel echo, then reduce background noise and even out loud and quiet speech, each an Off, Gentle or Strong select), then the join and leave sounds. A row whose controls do not fit beside its line puts them under it.",
        usage: "VoiceSection::new(&input, &output, &controls, Meter::Level(mic_level.read()), &settings, on_change)
    .gain(mic_volume.read())
    .monitor(monitor.is_on())
device_select(&endpoints, &chosen, window, cx)",
        render: voice_settings::voice_section,
    },
    Story {
        name: "Voice stats",
        description: "How your media reaches the SFU, refreshed each second: the path, round trip, loss, jitter and what is sent and received. Opened from your own Member panel while In Voice.",
        usage: "dialog::open(window, cx, |d, _, _| d.title(\"Voice stats\").child(body.clone()))
VoiceStatsList::new(session.state().and_then(|s| s.voice.stats.clone()))",
        render: voice::voice_stats,
    },
    Story {
        name: "Connection entry",
        description: "The live entry at the top of the Notification Center while the Lobby is not calm: a title, one sentence, and the action that helps. It cannot be dismissed and leaves by itself once the Lobby is connected. The bar's tint is the glance.",
        usage: "connection_notice(state, lobby_name, last_connected) // None while calm\nNotificationBell::new(..).connection(notice, on_action)",
        render: notifications::connection,
    },
    Story {
        name: "Notifications",
        description: "The bell with its dot, and the Notification Center it opens: the connection while it is not calm, requests to join, then what happened lately.",
        usage: "NotificationBell::new(rows, unseen, on_dismiss, open, on_open_change, on_action)\n    .decisions(decisions, on_decide)\n    .connection(notice, on_action)",
        render: notifications::notifications,
    },
    Story {
        name: "Lobby switcher",
        description: "The Lobby's mark and name in the title bar; its menu lists the Lobbies on this device and Add Lobby.",
        usage: "LobbySwitcher::new(current, entries, on_pick)\nLobbySwitcher::still(current) // while the window closes the Lobby",
        render: title_bar::switcher,
    },
    Story {
        name: "Feed rows",
        description: "The chat feed's rows: history, day and New dividers, a group's first and later Messages, one's own on its way, files, and the typing line.",
        usage: "feed_row(&rows, ix, older, &load, on_link, &cards, cx)\nTypingLine::new(line)",
        render: feed::feed,
    },
    Story {
        name: "Attachments",
        description: "A Message's files: a picture at the box its preview reserves (skeleton, failed, shown), any other file as a compact card.",
        usage: "attachment_cards(&m.key, &m.attachments, cards, cx)",
        render: feed::attachments,
    },
    Story {
        name: "Composer",
        description: "The composer's formatting bar (a base Toolbar): each format's state and the compact bar of a folded window.",
        usage: "FormatBar::new(on_format).compact(narrow).active(self.formats_at_caret(value, cx))",
        render: feed::composer,
    },
    Story {
        name: "Onboarding",
        description: "Every step into a Lobby on one layout: the rail of steps (a done chip goes back), the question, one large field with its button, the reserved feedback line and one button back. Connect, Email, Code, Name, Waiting and Closed, at the default and the small window.",
        usage: "AuthScreen::new(StepRail::new(view.rail).on_go(go_to), title, subtitle)\n    .child(control_row(step_input(&self.email, \"email\"), primary_button(\"send-code\", label, busy_label, busy)))\n    .child(feedback_line(said, cx))\n    .link(back_button(\"auth-back\", back_to(lobby), cx).on_click(on_back))",
        render: auth::auth,
    },
    Story {
        name: "Toasts",
        description: "Notifications as they happen: one compact line in the main column, the whole message and its action on hover.",
        usage: "window.push_notification(issue_toast(&issue, cx.weak_entity()), cx)",
        render: notifications::toasts,
    },
    Story {
        name: "Empty states",
        description: "The feed before it has Messages: a Lobby not reached yet, the loading skeleton, a new Lobby's invite, and the calm empty state.",
        usage: "feed_placeholder(&history, lobby_name, invite, copied, unreached, roomy, view, cx)",
        render: feed::empty_states,
    },
    Story {
        name: "Source picker",
        description: "What to share: screens as tiles and windows as rows, each with a reserved picture box that its thumbnail fills as it arrives; while the list loads, and when capture is not available.",
        usage: "let view = cx.new(|cx| PickerView::new(on_pick, last_quality, cx));\nview.update(cx, |v, cx| v.set_model(model, cx));\nview.update(cx, |v, cx| v.set_thumbnail(&id, thumbnail, cx));",
        render: voice::source_picker,
    },
    Story {
        name: "Viewer",
        description: "A Screen Share on screen, another Member's or your own: in the panel above the feed or filling the view, with a toolbar over the picture's top for drawing, fullscreen and close, and Stop sharing in fullscreen. Viewers' ink lies over the picture.",
        usage: "ShareToolbar::new(ToolbarState::Mine { others_draw, blocked } | ToolbarState::Friend { pen }, ToolbarAt::Panel | ToolbarAt::Fullscreen { stop_sharing }, on_draw, on_fullscreen, on_close)\nShareView::new(name, feed, cx) | ShareView::own(feed, cx) // own: Session::own_share_feed\nShareView::set_place(Place::Panel(size) | Place::Fullscreen(size))\nShareView::set_ink(Some(InkLayer { board, authors: ink_authors(members), me, pen, at: None, spot: None }))",
        render: viewer::viewer,
    },
    Story {
        name: "Controls",
        description: "Lobby's controls over gpui-kit's base layer, in the app's theme: its buttons in each variant and state, and the small ones the components share.",
        usage: "ui::Button::new(\"voice-join\").variant(ui::Variant::Primary).size(ui::Size::Page).icon(IconName::Phone).label(\"Join Voice\")\nui::Switch::new(\"do-not-disturb\").checked(on).accessibility_label(\"Do Not Disturb\").on_change(on_change)\nui::Slider::new(&volume).accessibility_label(\"Volume\")",
        render: controls::controls,
    },
    Story {
        name: "Motion",
        description: "Each moment that moves, drawn by the app's own code, with a button that sets it off: a press, a popup, a dialog, a switch, a Message coming in, typing, speaking, the members sheet and a wrong Magic Code. Near-critical springs: Quick (~130 ms), Move (~200 ms) and Open (~235 ms), nothing past its mark.",
        usage: "ui::motion::{QUICK, MOVE, OPEN}\nReveal::new(id, open, window, cx).apply(surface, Side::Top)\nReveal::sheet(id, open, window, cx)\nmotion::arriving(row, motion::arrived(since)?)\nspring(id, target, motion::QUICK, window, cx)",
        render: motion::motion,
    },
];

thread_local! {
    static SAMPLES: Cell<usize> = const { Cell::new(0) };
}

/// A page shows one component many times, each copy with the same element ids, and a debug build
/// panics on the duplicate accessibility nodes that makes. So every sample sits in its own id,
/// numbered in render order, which is the same every frame.
pub(super) fn start_page() {
    SAMPLES.set(0);
}

fn sample_id() -> SharedString {
    let n = SAMPLES.get();
    SAMPLES.set(n + 1);
    format!("sample-{n}").into()
}

pub(super) fn wrap(children: impl IntoIterator<Item = AnyElement>) -> Div {
    div().flex().flex_wrap().items_end().gap_4().children(
        children
            .into_iter()
            .map(|c| div().id(sample_id()).flex().child(c).into_any_element()),
    )
}

pub(super) fn caption(
    label: impl Into<SharedString>,
    element: impl IntoElement,
    cx: &App,
) -> AnyElement {
    let label = label.into();
    div()
        .id(sample_id())
        .flex()
        .flex_col()
        .items_center()
        .gap_1p5()
        .child(element)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .into_any_element()
}

pub(super) fn panel(width: f32, surface: Hsla, cx: &App) -> Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .p_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .bg(surface)
        .flex()
        .flex_col()
}

/// What a kit `Popover` draws around its content; the Member panel asks for a tighter padding.
pub(super) fn popover_frame(child: impl IntoElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .self_start()
        .p_1()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .shadow_md()
        .child(child)
        .into_any_element()
}

pub(super) fn label(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
        .into_any_element()
}

pub(super) fn bar(children: impl IntoIterator<Item = AnyElement>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .h(px(crate::components::TITLE_BAR_HEIGHT))
        .w_full()
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .children(children)
        .into_any_element()
}
