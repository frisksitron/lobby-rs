pub mod address_form;
pub mod attachment_card;
pub mod attachment_dialog;
pub mod auth;
pub mod avatar;
pub mod bar_button;
pub mod code_input;
pub mod composer;
pub mod connection_notice;
pub mod context_menu;
pub mod controls;
pub mod draft_strip;
pub mod feed;
pub mod floating_sheet;
pub mod icon_disc;
pub mod ink;
pub mod ink_overlay;
pub mod lobby_switcher;
pub mod media_images;
pub mod member_avatar;
pub mod member_panel;
pub mod member_row;
pub mod mention_chip;
pub mod mention_menu;
pub mod mic_meter;
pub mod notification_center;
pub mod notification_entry;
pub mod section_label;
pub mod share_controls;
pub mod share_view;
pub mod source_picker;
pub mod title_bar;
pub mod tokens;
pub mod truncated_text;
pub mod update_pill;
pub mod voice_bar;
pub mod voice_face;
pub mod voice_foot;
pub mod voice_stats;
pub mod watch_tile;
pub mod word_button;

use std::rc::Rc;

use gpui_kit::{App, Window};

/// What a child component calls with a value when the person acts.
pub type Callback<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;

pub use address_form::{ADD_LOBBY_IDS, AddressForm, CONNECT_IDS};
pub use attachment_card::{
    CardAction, Cards, attachment_cards, card_id, picture_box, previews, size_label,
};
pub use attachment_dialog::AttachmentDialog;
pub use auth::{
    AuthScreen, Feedback, STEP_MARK, StepRail, back_button, busy_line, busy_screen, control_row,
    feedback_line, inline_row, primary_button, quiet_link, status_mark, tag, waiting_mark,
};
pub use avatar::{Avatar, initials};
pub use bar_button::BarButton;
pub use code_input::{CodeInput, CodeTyped, CodeTyping};
pub use composer::{FORMAT_BAR_HEIGHT, FormatBar, format_for};
pub use connection_notice::{
    ConnectionAction, ConnectionNotice, ConnectionTone, bar_tint, connection_notice, tone_color,
};
pub use controls::{
    confirm_alert, page_button, page_icon_button, page_select, step_button, step_input,
};
pub use draft_strip::{DRAFT_STRIP_H, draft_strip};
pub use feed::{
    ChangeMessage, Feed, FeedAction, FeedRow, MessageChange, MessageRow, Pending, card_max_w,
    feed_row, feed_rows, typing_indicator,
};
pub use floating_sheet::floating_sheet;
pub use icon_disc::IconDisc;
pub use lobby_switcher::{
    IconOf, LobbyAction, LobbySwitcher, SwitcherCurrent, SwitcherEntry, SwitcherPick, shown_address,
};
pub use media_images::{MediaImages, PictureOf, Slot, picture, pictures};
pub use member_avatar::{AvatarState, MemberAvatar};
pub use member_panel::{
    MEMBER_CARD_PAD, MemberPanel, MemberPanelModel, OwnerAction, PanelActions, PanelStep, Rename,
    Said, WatchLine,
};
pub use member_row::{MemberRow, MemberRowActions, MemberRowModel, voice_box};
pub use mention_menu::mention_menu;
pub use mic_meter::{Meter, MicMeterBar, lit_segments, mic_hint};
pub use notification_center::{
    ActionContext, Decide, DecisionRow, FOLD_FOR, NotificationBell, NotificationRow, SETTLE_FOR,
    Settled, action_button, decision_rows, notification_icon, notification_rows, row_action,
};
pub use notification_entry::{Detail, LEAD_SIZE, Lead, NotificationEntry};
pub use section_label::section_label;
pub use share_controls::Pointer;
pub use share_view::{
    Place, ShareToolbar, ShareView, Sharer, ToolbarAt, ToolbarState, Whose, YOU, esc_hint,
    fullscreen_frame, panel_fit, panel_frame, strain_notice,
};
pub use source_picker::{PICKER_W, PickerModel, PickerView};
pub use title_bar::{AppTitleBar, TITLE_BAR_HEIGHT, mark_color, mark_slots, slot_color};
pub use tokens::{
    AvatarSize, BAR_CONTROL_H, BORDER, FIELD_H, ICON_MD, ICON_SM, ICON_XS, INLINE_H, LINE_H,
    MEMBER_ROW_GAP, PAGE_CONTROL_H, PAGE_CONTROL_RADIUS, PERCENT_W, PULSE_FPS, READOUT_GAP,
    ROW_HEIGHT, SURFACE_INNER_RADIUS, SURFACE_PAD, SURFACE_RADIUS, idle_color, live_red,
    nested_radius, picture_backdrop, voice_accent,
};
pub use truncated_text::truncated_text;
pub use update_pill::UpdatePill;
pub use voice_bar::{
    ShareControl, VoiceBarActions, VoiceBarModel, VoiceStrip, Waits, card_surface, voice_bar_model,
};
pub use voice_face::VoiceFace;
pub use voice_foot::{FootSize, VoiceFoot};
pub use voice_stats::{
    STATS_DIALOG_W, VoiceStatsDialog, VoiceStatsList, stats_dialog_width, stats_lines,
};
pub use watch_tile::{Tile, TilePaint, TilePress, WatchTile};
pub use word_button::small_word_button;
