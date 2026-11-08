//! Opening a link from a Message: only what `lobby_client::links::external_url` allows reaches the
//! system browser. A driven or tested app records the links instead (the `LinkRecorder` global).

use std::{cell::RefCell, rc::Rc};

use gpui_kit::{App, Global};
use lobby_client::links::external_url;
use tracing::{info, warn};

#[derive(Clone, Default)]
pub struct LinkRecorder(pub Rc<RefCell<Vec<String>>>);

impl Global for LinkRecorder {}

pub fn open_external(href: &str, cx: &mut App) {
    let Some(url) = external_url(href) else {
        warn!(
            scheme = href.split(':').next().unwrap_or(""),
            "link refused"
        );
        return;
    };
    if let Some(recorder) = cx.try_global::<LinkRecorder>() {
        recorder.0.borrow_mut().push(url.to_string());
        return;
    }
    info!(scheme = url.scheme(), "opening a link");
    cx.open_url(url.as_str());
}

pub fn recorded(cx: &App) -> Vec<String> {
    cx.try_global::<LinkRecorder>()
        .map(|r| r.0.borrow().clone())
        .unwrap_or_default()
}
