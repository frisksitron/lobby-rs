//! Real GPUI frames without an OS window: a `HeadlessAppContext` with the wgpu renderer and Cosmic
//! text (the bundled Figtree, so every machine draws the same glyphs) renders the real `Shell`.
//! `shot` writes 1x element crops and `dump` the element snapshots plus the model, under
//! `target/ui-shots/<test>/`. Needs a Vulkan or GL driver, so the tests that use it run only with
//! `LOBBY_RENDER_TESTS=1`.
//!
//! This is wgpu + Cosmic, not the shipped DirectX + DirectWrite path: it proves layout, colour,
//! icons and stacking.
#![allow(dead_code)]

use std::{
    borrow::Cow,
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::{
    AnyWindowHandle, Entity, HeadlessAppContext, Pixels, PlatformHeadlessRenderer,
    PlatformTextSystem as _, Size, px, size,
};
use lobby_client::{Client, HttpPolicy, MemoryTokenStore, ReconnectPolicy};
use lobby_desktop::{Shell, TokioRuntime, assets::LobbyAssets, automation, shell::WindowSetup};

pub const DEFAULT: Size<Pixels> = lobby_desktop::window_bounds::DEFAULT_SIZE;

pub fn enabled() -> bool {
    std::env::var("LOBBY_RENDER_TESTS").is_ok_and(|v| v == "1")
}

pub fn headless(render: bool) -> HeadlessAppContext {
    let text = gpui_wgpu::CosmicTextSystem::new_without_system_fonts(lobby_desktop::fonts::FAMILY);
    text.add_fonts(
        lobby_desktop::fonts::FACES
            .iter()
            .map(|face| Cow::Borrowed(*face))
            .collect(),
    )
    .expect("the bundled fonts load");
    let mut cx =
        HeadlessAppContext::with_platform(Arc::new(text), Arc::new(LobbyAssets), move || {
            if !render {
                return Ok(None);
            }
            gpui_wgpu::WgpuHeadlessRenderer::new()
                .map(|r| Some(Box::new(r) as Box<dyn PlatformHeadlessRenderer>))
        });
    cx.update(|cx| cx.set_reduce_motion(true));
    cx
}

pub fn shots_dir(test: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/ui-shots")
        .join(test)
}

pub struct Rendered {
    pub cx: HeadlessAppContext,
    pub window: AnyWindowHandle,
    pub shell: Entity<Shell>,
    pub exits: Rc<Cell<u32>>,
    out: PathBuf,
    prefix: String,
}

pub struct Opening {
    pub size: Size<Pixels>,
    pub dir: Rc<tempfile::TempDir>,
    pub store: MemoryTokenStore,
    pub reconnect: Option<ReconnectPolicy>,
    pub render: bool,
    pub focused: bool,
    pub out: PathBuf,
    pub prefix: String,
}

pub fn open(rt: &tokio::runtime::Runtime, o: Opening) -> Rendered {
    let mut cx = headless(o.render);
    let mut client = Client::new(
        rt.handle().clone(),
        Arc::new(o.store.clone()),
        HttpPolicy::LoopbackOnly,
    )
    .unwrap()
    .with_clock(automation::GpuiClock::of(cx.background_executor().clone()));
    if let Some(policy) = o.reconnect {
        client = client.with_reconnect_policy(policy);
    }
    let deps = super::shell_deps(
        client,
        o.dir.path(),
        &o.store,
        WindowSetup {
            size: o.size,
            origin: Some(gpui_kit::point(px(0.), px(0.))),
            focus: false,
            show: true,
            remember: false,
            ..WindowSetup::default()
        },
        Rc::new(|_, _, _| {}),
    );
    let (exit, exits) = super::exit_counter();
    let handle = rt.handle().clone();
    let (window, shell) = cx.update(|cx| {
        cx.set_global(TokioRuntime::borrowed(handle));
        gpui_kit::init(cx);
        lobby_desktop::ui::init(cx);
        cx.set_global(lobby_desktop::links::LinkRecorder::default());
        cx.set_global(lobby_desktop::os_toasts::OsToastRecorder::default());
        cx.set_global(lobby_desktop::files::FileRecorder::default());
        cx.set_global(lobby_desktop::clipboard::CopyRecorder::default());
        Shell::open(deps, exit, cx).unwrap()
    });
    cx.update(|cx| {
        shell.update(cx, |s, _| {
            s.set_idle_source(|| None);
            s.keep_in_tray(None);
        })
    });
    if o.focused {
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
    }
    cx.run_until_parked();
    Rendered {
        cx,
        window,
        shell,
        exits,
        out: o.out,
        prefix: o.prefix,
    }
}

impl Rendered {
    pub fn settle(&mut self) {
        let shell = self.shell.clone();
        let mut waited = 0;
        loop {
            self.cx.run_until_parked();
            if self.cx.update(|cx| super::caught_up(&shell, cx)) {
                break;
            }
            waited += 1;
            assert!(
                waited < 5000,
                "the window never showed the Intents it dispatched"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.cx
            .update_window(self.window, |_, window, cx| {
                use gpui_kit::test::TestWindowExt as _;
                window.render_frame(cx);
            })
            .unwrap();
    }

    pub fn try_shot(&mut self, step: &str, id: Option<&str>) -> Result<PathBuf, String> {
        self.settle();
        let (bounds, scale) = self
            .cx
            .update_window(self.window, |_, window, _| {
                let bounds = id.map(|id| automation::element_bounds(window, id));
                (bounds, window.scale_factor())
            })
            .unwrap();
        if let Some(None) = bounds {
            return Err(format!("{:?} is not drawn", id.unwrap_or_default()));
        }
        let image = self
            .cx
            .capture_screenshot(self.window)
            .map_err(|_| "no renderer (LOBBY_RENDER_TESTS=1 draws frames)")?;
        let crop = match bounds.flatten() {
            Some(b) => automation::crop(&image, b, scale),
            None => automation::downscale(&image, scale),
        };
        let path = self.out.join(format!("{}{step}.png", self.prefix));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        crop.save(&path).map_err(|e| e.to_string())?;
        Ok(path)
    }
}

pub fn wh(w: f32, h: f32) -> Size<Pixels> {
    size(px(w), px(h))
}
