//! The feed before it has Messages: the skeleton while the newest page loads, the invite of a new
//! Lobby, a Lobby not reached yet, and the calm empty state.

use gpui_kit::{
    AnyElement, App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, TestSupportExt as _, WeakEntity,
    assets::IconName,
    component::{
        ActiveTheme as _, Disableable as _, Icon, Sizable as _,
        button::{Button, ButtonVariants as _},
        empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle},
        skeleton::Skeleton,
    },
    div,
    prelude::FluentBuilder as _,
    relative,
};

use super::LobbyView;
use crate::components::{
    Avatar, AvatarSize, BORDER, ConnectionAction, ConnectionNotice, ICON_SM, IconDisc,
    nested_radius, voice_accent,
};
use crate::scale::{device_in, u};

const EMPTY_TEXT_MAX: f32 = 420.;

/// The invite's address box, the padding at its Copy button's sides and the button's corners.
const INVITE_RADIUS: f32 = 8.;
const INVITE_PAD: f32 = 4.;
const COPY_RADIUS: f32 = nested_radius(INVITE_RADIUS, BORDER + INVITE_PAD);

/// Skeleton rows while the newest page loads, sized like the real ones so nothing jumps. The
/// first Member alone in a new Lobby gets the invite instead: the Lobby's address with a Copy
/// button. Not `roomy`: a watched Screen Share fills the column.
#[allow(clippy::too_many_arguments)]
pub(crate) fn feed_placeholder(
    history: &lobby_client::History,
    lobby: &str,
    invite: Option<SharedString>,
    copied: bool,
    unreached: Option<(Hsla, ConnectionNotice)>,
    roomy: bool,
    view: WeakEntity<LobbyView>,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let loading = matches!(
        history,
        lobby_client::History::Loading | lobby_client::History::NotLoaded
    );
    if let Some((color, notice)) = unreached.filter(|_| loading) {
        return unreached_lobby(lobby, color, &notice, view);
    }
    if loading {
        return loading_rows();
    }
    let lobby = if lobby.is_empty() { "the Lobby" } else { lobby };
    if let Some(address) = invite {
        return invite_people(lobby, address, copied, view, cx);
    }
    if !roomy {
        return hello_note(lobby, muted);
    }
    empty_state(
        "feed-empty",
        IconDisc::new(IconName::MessageCircle, 48.).hello("feed-empty-hello"),
        lobby_client::copy::NO_MESSAGES_YET.to_owned(),
        lobby_client::copy::say_hello(lobby),
        None,
    )
}

/// A Lobby not reached since it was opened: its mark, dimmed, and what the Notification Center's
/// connection entry says, in the same words and with the same buttons.
fn unreached_lobby(
    lobby: &str,
    color: Hsla,
    notice: &ConnectionNotice,
    view: WeakEntity<LobbyView>,
) -> AnyElement {
    let button = |action: ConnectionAction, primary: bool, enabled: bool| {
        let view = view.clone();
        let id = format!("feed-{}", action.id());
        Button::new(id.clone())
            .debug_selector(move || id.clone())
            .small()
            .label(action.label())
            .when(action != ConnectionAction::ChangeLobby, |b| {
                b.icon(Icon::new(IconName::RotateCw))
            })
            .outline()
            .disabled(!enabled)
            .loading(primary && !enabled)
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |v, cx| v.connection_action(action, window, cx));
            })
    };
    let buttons = div()
        .flex()
        .gap_2()
        .children(
            notice
                .action
                .map(|a| button(a, true, notice.action_enabled)),
        )
        .children(notice.secondary.map(|a| button(a, false, true)));
    empty_state(
        "feed-unreached",
        div()
            .opacity(0.55)
            .child(Avatar::lobby(lobby.to_owned(), color).size(AvatarSize::Hero)),
        notice.title.to_string(),
        notice.detail.to_string(),
        Some(EmptyContent::new().child(buttons)),
    )
}

/// The rows' own geometry, so the Messages land where the skeleton was.
fn loading_rows() -> AnyElement {
    use crate::components::feed::{
        FOOT_ROOM, GROUP_GAP, HEADER_HEIGHT, LINE_HEIGHT, MESSAGE_AVATAR, ROW_PAD_Y, TEXT_SIZE,
    };
    div()
        .id("feed-loading")
        .size_full()
        .flex()
        .flex_col()
        .justify_end()
        .pb(u(FOOT_ROOM))
        .children([0.55_f32, 0.8, 0.4].into_iter().enumerate().map(|(i, w)| {
            div()
                .id(SharedString::from(format!("feed-skeleton-row-{i}")))
                .w_full()
                .px_2()
                .pt(u(GROUP_GAP))
                .child(
                    div()
                        .px_2()
                        .py(u(ROW_PAD_Y))
                        .flex()
                        .gap_2()
                        .child(
                            Skeleton::new()
                                .flex_shrink_0()
                                .size(u(MESSAGE_AVATAR.px()))
                                .rounded_full(),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(u(1.))
                                .child(
                                    div()
                                        .h(u(HEADER_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .child(Skeleton::new().h(u(10.)).w(u(96.))),
                                )
                                .child(
                                    div()
                                        .h(u(TEXT_SIZE * LINE_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .child(Skeleton::new().h(u(12.)).w(relative(w))),
                                ),
                        ),
                )
                .test_support()
        }))
        .into_any_element()
}

fn invite_people(
    lobby: &str,
    address: SharedString,
    copied: bool,
    view: WeakEntity<LobbyView>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let (border, background, fg) = (theme.border, theme.background, theme.foreground);
    empty_state(
        "feed-invite",
        IconDisc::new(IconName::UserPlus, 48.).accent(voice_accent(theme)),
        format!("{lobby} is ready. Invite people"),
        "Send them this address. Their requests to join show up under the bell.".to_owned(),
        Some(
            EmptyContent::new().max_w(u(420.)).child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_3()
                    .pr(u(INVITE_PAD))
                    .py(u(INVITE_PAD))
                    .rounded(u(INVITE_RADIUS))
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .child(
                        div()
                            .debug_selector(|| "invite-address".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_left()
                            .text_color(fg)
                            .child(address),
                    )
                    .child(
                        Button::new("copy-invite")
                            .small()
                            .rounded(device_in(COPY_RADIUS, cx))
                            .when(!copied, |b| b.outline())
                            .when(copied, |b| b.ghost())
                            .icon(Icon::new(if copied {
                                IconName::Check
                            } else {
                                IconName::Copy
                            }))
                            .label(if copied { "Copied" } else { "Copy" })
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |v, cx| v.copy_invite(cx));
                            }),
                    ),
            ),
        ),
    )
}

/// One line, while a watched Screen Share takes the column.
fn hello_note(lobby: &str, muted: Hsla) -> AnyElement {
    div()
        .id("feed-empty")
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_end()
        .px_6()
        .pb_2()
        .child(
            div()
                .id("feed-empty-note")
                .max_w_full()
                .flex()
                .items_center()
                .gap_1p5()
                .text_sm()
                .text_color(muted)
                .child(
                    Icon::new(IconName::MessageCircle)
                        .size(u(ICON_SM))
                        .text_color(muted),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(lobby_client::copy::say_hello(lobby)),
                )
                .test_support(),
        )
        .test_support()
        .into_any_element()
}

fn empty_state(
    id: &'static str,
    media: impl IntoElement,
    title: String,
    description: String,
    content: Option<EmptyContent>,
) -> AnyElement {
    let empty = Empty::new().gap_4().p_0().px_6().header(
        EmptyHeader::new()
            .max_w(u(EMPTY_TEXT_MAX))
            .media(EmptyMedia::new().mb_1().child(media))
            .title(
                EmptyTitle::new()
                    .w_full()
                    .line_clamp(2)
                    .text_base()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .description(EmptyDescription::new().child(description)),
    );
    div()
        .id(id)
        .debug_selector(move || id.into())
        .size_full()
        .flex()
        .flex_col()
        .child(match content {
            Some(content) => empty.content(content),
            None => empty,
        })
        .test_support()
        .into_any_element()
}
