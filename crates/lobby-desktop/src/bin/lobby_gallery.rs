//! The component gallery window. No Lobby and no Tokio runtime: components draw from plain models.

use lobby_desktop::{
    assets::LobbyAssets,
    gallery::{Gallery, STORIES, find},
};

fn main() -> anyhow::Result<()> {
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
    gpui_kit::application()
        .with_assets(LobbyAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.activate(true);
            if let Err(e) = Gallery::open(initial, cx) {
                eprintln!("cannot open the gallery window: {e}");
                cx.quit();
            }
        });
    Ok(())
}
