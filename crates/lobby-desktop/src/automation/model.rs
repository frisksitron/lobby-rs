//! The dump: what the window shows, as JSON, for scenario predicates and transcripts. It is built
//! from the same view models the views render, so a check reads what a person would see.

use gpui_kit::{App, Entity, Window, base::test_support::snapshots};
use lobby_client::{Chip, ChipAt, Phase, Variant, picture};
use serde_json::{Map, Value, json};

use crate::{
    components::{
        FeedRow, Feedback, Rename, ShareControl, Tile, VoiceBarModel, Whose, card_id,
        notification_rows, picture_box, stats_lines, voice_bar_model,
    },
    files::FilePrompt,
    lobby_view::{LobbyView, SidebarLayout},
    shell::{QuitState, Route, Shell},
};

pub fn dump(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> Value {
    let dialog = crate::kit::WindowExt::has_active_dialog(window, cx);
    let mut out = model(shell.read(cx), window, cx);
    out["window"]["dialog"] = Value::Bool(dialog);
    out["files"] = files(cx);
    out["clipboard"] = json!(crate::clipboard::recorded(cx));
    out["elements"] = elements(window);
    out
}

fn chip(chip: &Chip) -> Value {
    let stage = format!("{:?}", chip.stage);
    match &chip.at {
        ChipAt::Done { value, back } => {
            json!({"stage": stage, "at": "done", "value": value, "back": back})
        }
        ChipAt::Current => json!({"stage": stage, "at": "current"}),
        ChipAt::Ahead => json!({"stage": stage, "at": "ahead"}),
    }
}

fn files(cx: &App) -> Value {
    let Some(log) = crate::files::recorded(cx) else {
        return Value::Null;
    };
    let paths = |paths: &[std::path::PathBuf]| -> Vec<String> {
        paths.iter().map(|p| p.display().to_string()).collect()
    };
    let prompts: Vec<Value> = log
        .prompts
        .iter()
        .map(|p| match p {
            FilePrompt::Pick => json!({"kind": "pick"}),
            FilePrompt::Save { suggested } => json!({"kind": "save", "suggested": suggested}),
        })
        .collect();
    json!({
        "prompts": prompts,
        "opened": paths(&log.opened),
        "revealed": paths(&log.revealed),
        "saved": paths(&log.saved),
        "unmarked": log.saved.iter().filter(|p| !marked(p)).map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
    })
}

/// Whether a saved file carries the downloaded-from-the-internet mark (Mark-of-the-Web).
fn marked(path: &std::path::Path) -> bool {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::path::Path::new(&stream).is_file()
}

pub fn elements(window: &Window) -> Value {
    let mut map = Map::new();
    let mut all = snapshots(window);
    all.sort_by(|a, b| {
        let (a, b) = (a.bounds().origin, b.bounds().origin);
        (b.y, b.x)
            .partial_cmp(&(a.y, a.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for s in all {
        let Some(id) = s.path().last() else { continue };
        let b = s.bounds();
        map.insert(
            id.to_string(),
            json!({
                "bounds": [f32::from(b.origin.x), f32::from(b.origin.y),
                           f32::from(b.size.width), f32::from(b.size.height)],
                "visible": s.visible(),
                "disabled": s.disabled(),
                "label": s.label(),
                "value": s.value(),
                "role": s.role().map(|r| format!("{r:?}")),
            }),
        );
    }
    Value::Object(map)
}

fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Connecting { .. } => "connecting",
        Phase::Connected => "connected",
        Phase::Reconnecting { .. } => "reconnecting",
        Phase::Offline { .. } => "offline",
        Phase::Unavailable { .. } => "unavailable",
        Phase::ProtocolMismatch => "protocol_mismatch",
        Phase::NeedsAuth => "needs_auth",
        Phase::SessionReplaced => "session_replaced",
    }
}

/// The saved settings, what the window shows for them, and whether the page is open.
fn settings(shell: &Shell, cx: &App) -> Value {
    let saved = shell.lobbies.read(cx).settings();
    json!({
        "open": shell.settings.is_some(),
        "close_to_tray": saved.close_to_tray,
        "ui_scale": saved.ui_scale,
        "sounds": saved.sounds,
        "share_quality": lobby_client::copy::quality_label(saved.share_quality),
        "member_volumes": saved.member_volumes.values().map(|m| m.len()).sum::<usize>(),
        "tray": shell.has_tray(),
        "updates": crate::updates::AppUpdates::entity(cx)
            .map(|u| format!("{:?}", u.read(cx).status())),
        "voice":shell.settings.as_ref().and_then(|s| s.read(cx).voice()).map(|v| v.read(cx).dump(cx)),
        "input_device": format!("{:?}", saved.input_device),
        "output_device": format!("{:?}", saved.output_device),
        "shown": {
            "theme": cx.global::<crate::kit::Theme>().theme_name().as_ref(),
            "dark": cx.global::<crate::kit::Theme>().is_dark(),
            "font_size": (f64::from(cx.global::<crate::kit::Theme>().font_size) * 100.).round() / 100.,
        },
    })
}

fn model(shell: &Shell, window: &Window, cx: &App) -> Value {
    let size = crate::scale::visible_size(window);
    let margin = crate::kit::window_paddings(window);
    let lobbies: Vec<Value> = shell
        .switcher_entries(cx)
        .into_iter()
        .map(|e| {
            json!({
                "origin": e.origin.to_string(),
                "name": e.name.as_ref(),
                "icon": e.icon.as_ref().map(ToString::to_string),
                "icon_shown": e.icon.as_ref().is_some_and(|i| shell.icons.read(cx).shown(&e.origin, i, cx)),
                "current": e.current,
                "signed_in": e.signed_in,
            })
        })
        .collect();
    let mut out = json!({
        "route": format!("{:?}", shell.route_name()),
        "quit": match shell.quit {
            QuitState::Running => "running",
            QuitState::Quitting => "quitting",
            QuitState::Done { .. } => "done",
        },
        "window": {
            "width": f32::from(size.width),
            "height": f32::from(size.height),
            // Where the visible window starts in the surface the element bounds are measured in.
            "margin": [f32::from(margin.left), f32::from(margin.top)],
            "active": window.is_window_active(),
            "hidden": shell.hidden_to_tray(),
        },
        "lobbies": lobbies,
        "add_lobby_open": shell.add_lobby.is_some(),
        "add_lobby_url": shell.add_lobby.as_ref().map(|v| v.read(cx).url.read(cx).value().to_string()),
        "settings": settings(shell, cx),
        "switch_asks": shell.pending_switch.is_some(),
        "errand": shell.errand_on().map(ToString::to_string),
        "issues":shell.issues.read(cx).issues().iter().map(|i| i.kind.key()).collect::<Vec<_>>(),
    });
    match &shell.route {
        Route::LobbyUrl(step) => {
            out["url_step"] = json!({
                "value": shell.url.read(cx).value().to_string(),
                "busy": step.busy(cx),
                "error": step.error(cx).map(|e| e.to_string()),
                "notice": step.notice.as_ref().map(|e| e.to_string()),
            });
        }
        Route::SignIn(sign_in) => {
            let auth = sign_in.auth.read(cx);
            let (tone, text) = match &auth.feedback(cx) {
                Some(Feedback::Error(t)) => ("error", Some(t.to_string())),
                Some(Feedback::Notice(t)) => ("notice", Some(t.to_string())),
                Some(Feedback::Info(t)) => ("info", Some(t.to_string())),
                Some(Feedback::Hint(t)) => ("hint", Some(t.to_string())),
                None => ("none", None),
            };
            out["auth"] = json!({
                "origin": sign_in.origin.to_string(),
                "lobby": auth.info(cx).name,
                "step": format!("{:?}", auth.step_name(cx)),
                "busy": auth.busy(cx),
                "feedback": {"tone": tone, "text": text},
                "email": auth.email.read(cx).value().to_string(),
                "name": auth.name.read(cx).value().to_string(),
                "rail": auth.rail().iter().map(chip).collect::<Vec<_>>(),
                "back_to": auth.back_to,
            });
        }
        Route::Lobby(route) => {
            out["lobby"] = lobby(route.origin.to_string(), &route.view, window, cx);
        }
    }
    out
}

fn lobby(origin: String, view: &Entity<LobbyView>, window: &Window, cx: &App) -> Value {
    let v = view.read(cx);
    let Some(state) = v.session_entity().read(cx).state() else {
        return json!({"origin": origin, "state": null});
    };
    let connection = v.connection_notice(cx);
    let groups = v.sidebar.read(cx).groups(state);
    let mut members = Map::new();
    let pictures = v.images.read(cx);
    let mut put = |section: &str, rows: &[crate::components::MemberRowModel]| {
        for r in rows {
            members.insert(
                r.name.to_string(),
                json!({
                    "section": section,
                    "avatar": r.avatar.as_ref().map(ToString::to_string),
                    "avatar_shown": r.avatar.as_ref().is_some_and(|a| pictures.shown(a, Variant::Square)),
                    "presence": r.presence,
                    "me": r.is_me,
                    "in_voice": r.voice.is_some(),
                    "joining": r.voice.is_some_and(|v| v.joining),
                    "muted": r.voice.is_some_and(|v| v.muted),
                    "deafened": r.voice.is_some_and(|v| v.deafened),
                    "speaking": r.voice.is_some_and(|v| v.speaking),
                    "sharing": r.sharing,
                    "tile": r.tile.map(|t| match t {
                        Tile::Faint => "faint",
                        Tile::Unavailable => "unavailable",
                        Tile::Rest => "rest",
                        Tile::Coming => "coming",
                        Tile::OnScreen { .. } => "on_screen",
                    }),
                    "owner": r.owner,
                }),
            );
        }
    };
    put("voice", &groups.in_voice);
    put("online", &groups.online);
    put("offline", &groups.offline);
    // The rendered Messages alone, without the day and history rows of `chat.rows`.
    let feed: Vec<Value> = v
        .feed
        .read(cx)
        .feed
        .rows()
        .iter()
        .filter_map(FeedRow::as_message)
        .map(|m| {
            json!({"author": m.author.as_ref(), "text": m.text.as_ref(), "pending": m.pending.is_some()})
        })
        .collect();
    let voice = match voice_bar_model(state) {
        VoiceBarModel::Idle { can_join } => json!({"controls": "join", "can_join": can_join}),
        VoiceBarModel::Connecting => json!({"controls": "connecting"}),
        VoiceBarModel::InVoice {
            muted,
            deafened,
            members,
            share,
            reconnecting,
            mute_nudges,
        } => json!({
            "controls": "in_voice",
            "muted": muted,
            "mute_nudges": mute_nudges,
            "deafened": deafened,
            "members": members,
            "reconnecting": reconnecting,
            "share": match share {
                ShareControl::Off => "off",
                ShareControl::Starting { .. } => "starting",
                ShareControl::Live { .. } => "live",
            },
        }),
    };
    let ctx = v.action_context(cx);
    let center = v.issues_center(cx);
    let notifications: Vec<Value> = notification_rows(center, ctx)
        .into_iter()
        .map(|r| {
            json!({
                "code": r.issue.kind.key(),
                "severity": format!("{:?}", r.issue.severity()).to_lowercase(),
                "title": r.title.as_ref(),
                "message": r.message.as_ref(),
                "unseen": r.unseen,
                "action": r.action.map(|a| a.label),
            })
        })
        .collect();
    json!({
        "origin": origin,
        "name": v.lobby_name(cx),
        "phase": phase_name(&state.phase),
        "reconnects": v.reconnects(),
        "me": state.my_username().map(|u| u.to_string()),
        "connection": connection.map(|n| json!({
            "tone": format!("{:?}", n.tone).to_lowercase(),
            "title": n.title.as_ref(),
            "detail": n.detail.as_ref(),
            "action": n.action.map(|a| a.label()),
            "action_enabled": n.action_enabled,
            "secondary": n.secondary.map(|a| a.label()),
            "toast": n.toast,
        })),
        "members": members,
        "member_panel": v.sidebar.read(cx).open_panel_model(cx).map(|m| json!({
            "user": m.name.as_ref(),
            "owner": m.owner,
            "since": m.since.as_ref(),
            "volume": (!m.is_me).then(|| v.sidebar.read(cx).open_panel_volume(cx)),
            "own": m.is_me.then(|| json!({
                "email": m.email.as_deref(),
                "dnd": m.dnd,
                "picture": {"set": m.has_picture, "busy": m.picture_busy},
                "name": match &m.rename {
                    Rename::Idle => json!({"editing": false}),
                    Rename::Editing { can_save, saving, said } => json!({
                        "editing": true,
                        "typed": v.sidebar.read(cx).open_panel_typed(cx),
                        "can_save": can_save,
                        "saving": saving,
                        "said": said.as_ref().map(|s| s.text.as_ref()),
                    }),
                },
            })),
        })),
        "voice_count": groups.in_voice.len(),
        "feed": feed,
        "chat": chat(v, state, cx),
        "links_opened": crate::links::recorded(cx),        "os_toasts": crate::os_toasts::recorded(cx).iter().map(|t| json!({
            "tag": t.tag,
            "title": t.title,
            "body": t.body,
        })).collect::<Vec<_>>(),
        "composer": v.composer.read(cx).value(cx).to_string(),
        "composer_editing": v.composer.read(cx).is_editing(),
        "formats_on": v.composer.read(cx).formats_on(cx).iter().map(|f| f.label()).collect::<Vec<_>>(),
        "voice": voice,
        "sharing": state.share.sharing.as_ref().map(|s| s.name.clone()),
        "watching": state.share.watch.as_ref().and_then(|w| {
            state.members.get(&w.sharer).map(|m| m.username.to_string())
        }),
        "watch_live": state.share.watch.as_ref().map(|w| w.live),
        "watch_switching": state.share.watch.as_ref().map(|w| w.coming.is_some()),
        "viewer": v.viewing.shown().map(|shown| {
            let stats = shown.view.read(cx).stats();
            let whose = match shown.whose(state) {
                Some(Whose::Mine) => Some("mine".to_owned()),
                Some(Whose::Member(id)) => state.members.get(&id).map(|m| m.username.to_string()),
                None => None,
            };
            json!({"whose": whose, "shown": stats.shown, "decoded": stats.decoded,
                   "fps": stats.fps, "size": stats.size.map(|(w, h)| [w, h]),
                   "strain": shown.view.read(cx).strain().map(|w| json!({
                       "title": lobby_client::copy::strain_title(&w),
                       "detail": lobby_client::copy::strain_detail(&w),
                   })),
                   "fullscreen": v.viewing.is_fullscreen(),
                   "esc_hint": v.viewing.hint_shown(),
                   "controls": shown.view.read(cx).controls_shown(window, cx),
                   "pen": shown.view.read(cx).pen(),
                   "ink": painted_ink(&shown.view, cx)})
        }),
        "sharers": crate::lobby_view::sharers(state).iter().map(|s| s.name.to_string())
            .collect::<Vec<_>>(),
        "ink": ink(v, state, cx),
        "drafts": state.drafts.items().iter().map(|d| {
            let (state, sent, reason) = match &d.state {
                lobby_client::DraftState::Queued => ("queued", 0, None),
                lobby_client::DraftState::Uploading { sent, .. } => ("uploading", *sent, None),
                lobby_client::DraftState::Ready(_) => ("ready", d.size, None),
                lobby_client::DraftState::Failed { reason, .. } => ("failed", 0, Some(reason)),
            };
            json!({"name": d.name, "size": d.size, "state": state, "sent": sent, "reason": reason})
        }).collect::<Vec<_>>(),
        "send_enabled": v.composer.read(cx).send_block(cx).is_none(),
        "picker_open": v.picker.is_some(),
        "voice_stats": v.voice_stats.as_ref().map(|_| match &state.voice.stats {
            Some(s) => json!(stats_lines(s)),
            None => json!("measuring"),
        }),
        "attachment_dialog": attachment_dialog(v, cx),
        "members_open": v.sidebar.read(cx).is_open(),
        "sidebar_width": v.sidebar.read(cx).shown(window, cx),
        "sidebar_collapsed": v.sidebar.read(cx).layout(window, cx) == SidebarLayout::Collapsed,
        "center_open": v.center_open,
        "unseen": center.unseen(),
        "notifications": notifications,
        "decisions": v.decisions(cx).iter().map(|d| json!({
            "name": d.username.as_ref(),
            "deciding": d.stage.deciding(),
            "settled": d.stage.settled().map(|s| format!("{s:?}")),
        })).collect::<Vec<_>>(),
    })
}

/// The strokes on every board, as a person would see them at the app's clock. `fade` is `full` or
/// `fading`; a stroke that faded out is not listed.
fn ink(v: &LobbyView, state: &lobby_client::LobbyState, cx: &App) -> Value {
    let now = cx.background_executor().now();
    let name = |user: &lobby_protocol::UserId| {
        state
            .members
            .get(user)
            .map_or_else(|| user.to_string(), |m| m.username.to_string())
    };
    let strokes: Vec<Value> = state
        .ink
        .iter()
        .flat_map(|(share, board)| {
            board.strokes(now).into_iter().map(move |s| {
                json!({
                    "share": share.get(),
                    "author": name(s.author),
                    "stroke": s.id.0,
                    "runs": s.runs()
                        .map(|run| run.points.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>())
                        .collect::<Vec<_>>(),
                    "points": s.runs().map(|run| run.points.len()).sum::<usize>(),
                    "lifted": s.lifted,
                    "fade": if s.opacity >= 1.0 { "full" } else { "fading" },
                })
            })
        })
        .collect();
    let own_drawing = state
        .me
        .as_ref()
        .and_then(|me| state.share_of(me))
        .is_some_and(|share| state.may_draw(share.share_id));
    json!({
        "can_draw": state.pen_share().is_some(),
        "drawing_on": own_drawing,
        "pen_down": v.session_entity().read(cx).pen_is_down(),
        "strokes": strokes,
        "overlay": v.overlay_dump(cx).map(|(over, state, shown, panes)| {
            json!({"over": over, "state": state, "shown": shown, "panes": panes})
        }),
    })
}

/// What the view on screen paints over its picture at the app's clock: each stroke's author,
/// colour and fade, and the names riding at the tips of strokes being drawn.
fn painted_ink(view: &Entity<crate::components::ShareView>, cx: &App) -> Value {
    let strokes = view.read(cx).ink(cx.background_executor().now());
    let hex = |c: gpui_kit::Hsla| {
        let c = gpui_kit::Rgba::from(c);
        let byte = |v: f32| (v * 255.).round() as u8;
        format!("#{:02x}{:02x}{:02x}", byte(c.r), byte(c.g), byte(c.b))
    };
    json!({
        "strokes": strokes.iter().map(|s| json!({
            "author": s.author.as_ref(),
            "color": hex(s.color),
            "fade": if s.opacity >= 1.0 { "full" } else { "fading" },
        })).collect::<Vec<_>>(),
        "tags": strokes.iter().filter(|s| s.tagged).map(|s| s.author.as_ref())
            .collect::<Vec<_>>(),
    })
}

fn chat(v: &LobbyView, state: &lobby_client::LobbyState, cx: &App) -> Value {
    let images = v.images.read(cx);
    let feed = &v.feed.read(cx).feed;
    let max_w = feed.cards_max_w();
    let rows: Vec<Value> = feed
        .rows()
        .iter()
        .map(|r| match r {
            FeedRow::History => json!({"kind": "history"}),
            FeedRow::Day { label, .. } => json!({"kind": "day", "label": label.as_ref()}),
            FeedRow::New => json!({"kind": "new"}),
            FeedRow::Message(m) => json!({
                "kind": "message",
                "key": m.key.as_ref(),
                "author": m.author.as_ref(),
                "avatar": m.avatar.as_ref().map(ToString::to_string),
                "avatar_shown": m.avatar.as_ref().is_some_and(|a| images.shown(a, Variant::Square)),
                "deactivated": m.deactivated,
                "first": m.first,
                "time": m.time.as_ref(),
                "text": m.text.as_ref(),
                "markdown": m.markdown.as_ref(),
                "pending": m.pending.map(|p| format!("{p:?}").to_lowercase()),
                "edited": m.edited,
                "attachments": m.attachments.iter().enumerate().map(|(j, a)| {
                    let picture = picture(a);
                    json!({
                        "id": card_id(&m.key, j).as_ref(),
                        "name": a.name,
                        "mime": a.mime_type,
                        "size": a.size,
                        "preview": a.preview.map(|p| [p.width, p.height]),
                        "kind": if picture.is_some() { "picture" } else { "file" },
                        "box": picture.map(|p| {
                            let (w, h) = picture_box(p, max_w);
                            [w, h]
                        }),
                        "shown": images.shown(&a.id, Variant::Preview),
                    })
                }).collect::<Vec<_>>(),
            }),
        })
        .collect();
    let now = cx.background_executor().now();
    json!({
        "older": format!("{:?}", state.older).to_lowercase(),
        "rows": rows,
        "typing": v.session_entity().read(cx).typing_line(now),
        "unseen": feed.unseen(),
        "at_bottom": feed.at_bottom(cx),
    })
}

fn attachment_dialog(v: &LobbyView, cx: &App) -> Value {
    let Some(dialog) = &v.attachment_dialog else {
        return json!({"open": false});
    };
    let d = dialog.read(cx);
    json!({
        "open": true,
        "name": d.attachment.name,
        "can_open": d.can_open(),
        "saved": d.saved.as_ref().map(|p| p.display().to_string()),
    })
}
