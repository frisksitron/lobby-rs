//! The component gallery window. No Lobby and no Tokio runtime: components draw from plain models.

use lobby_desktop::{
    assets::LobbyAssets,
    gallery::{Gallery, STORIES, find},
    host,
};

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    host::exit_unless_wayland_session();
    let initial = match std::env::args().nth(1) {
        None => None,
        Some(name) => match find(&name) {
            Some(ix) => Some(ix),
            None => {
                let names: Vec<_> = STORIES.iter().map(|s| s.name).collect();
                anyhow::bail!("no story {name:?}; the stories are: {}", names.join(", "));
            }
        },
    };
    host::application().with_assets(LobbyAssets).run(move |cx| {
        gpui_kit::init(cx);
        lobby_desktop::ui::init(cx);
        cx.activate(true);
        if let Err(e) = Gallery::open(initial, cx) {
            eprintln!("cannot open the gallery window: {e}");
            cx.quit();
        }
    });
    Ok(())
}
