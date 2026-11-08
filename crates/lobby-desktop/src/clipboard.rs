//! Copying text out of a menu. A driven or tested app records the last text instead (the
//! `CopyRecorder` global): a drive must never overwrite the person's own clipboard.

use std::{cell::RefCell, rc::Rc};

use gpui_kit::{App, ClipboardItem, Global};

#[derive(Clone, Default)]
pub struct CopyRecorder(pub Rc<RefCell<Option<String>>>);

impl Global for CopyRecorder {}

pub fn copy_text(text: String, cx: &mut App) {
    if let Some(recorder) = cx.try_global::<CopyRecorder>() {
        *recorder.0.borrow_mut() = Some(text);
        return;
    }
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

pub fn recorded(cx: &App) -> Option<String> {
    cx.try_global::<CopyRecorder>()
        .and_then(|r| r.0.borrow().clone())
}
