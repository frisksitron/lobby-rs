//! Where the main window opens: the rectangle the person left it in, if it is still on a display,
//! else the default, centered. Pure: the displays are passed in, so every case is a plain test.

use gpui_kit::{Bounds, Pixels, Point, Size, WindowBounds, point, px, size};
use lobby_client::SavedWindow;

pub const DEFAULT_SIZE: Size<Pixels> = size(px(680.), px(480.));
/// The small window.
pub const MIN_SIZE: Size<Pixels> = size(px(480.), px(400.));

/// A saved window counts as on a display when more than this part of it is on that display's
/// usable area (the taskbar's strip is not).
const MOSTLY_ON: f32 = 0.5;

/// A display as the platform reports it, in GPUI's logical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    /// The usable area, taskbar excluded.
    pub area: Bounds<Pixels>,
    /// Stable across runs, unlike GPUI's display id.
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub size: Size<Pixels>,
    /// `None` centers on `screen`, or on the primary display when that is `None` too.
    pub origin: Option<Point<Pixels>>,
    pub maximized: bool,
    /// An index into the screens given to [`place`].
    pub screen: Option<usize>,
}

impl Placement {
    pub fn centered(screen: Option<usize>) -> Self {
        Self {
            size: DEFAULT_SIZE,
            origin: None,
            maximized: false,
            screen,
        }
    }
}

pub fn place(saved: Option<&SavedWindow>, screens: &[Screen]) -> Placement {
    let Some(saved) = saved else {
        return Placement::centered(None);
    };
    let saved_screen = saved
        .display
        .as_ref()
        .and_then(|uuid| screens.iter().position(|s| s.uuid.as_ref() == Some(uuid)));
    let wanted = size(
        px(saved.width as f32).max(MIN_SIZE.width),
        px(saved.height as f32).max(MIN_SIZE.height),
    );
    let rect = Bounds::new(point(px(saved.x as f32), px(saved.y as f32)), wanted);
    let covered = |s: &Screen| overlap(&rect, &s.area) / area(&rect);
    let screen = saved_screen
        .filter(|&ix| covered(&screens[ix]) > MOSTLY_ON)
        .or_else(|| {
            // The display the window was on is gone (or the layout moved): any display holding most
            // of it will do.
            (0..screens.len())
                .filter(|&ix| covered(&screens[ix]) > MOSTLY_ON)
                .max_by(|&a, &b| covered(&screens[a]).total_cmp(&covered(&screens[b])))
        });
    let Some(screen) = screen else {
        return Placement::centered(saved_screen);
    };
    let usable = screens[screen].area.size;
    Placement {
        size: size(
            wanted.width.min(usable.width),
            wanted.height.min(usable.height),
        )
        .max(&MIN_SIZE),
        origin: Some(rect.origin),
        maximized: saved.maximized,
        screen: Some(screen),
    }
}

fn area(b: &Bounds<Pixels>) -> f32 {
    f32::from(b.size.width) * f32::from(b.size.height)
}

fn overlap(a: &Bounds<Pixels>, b: &Bounds<Pixels>) -> f32 {
    area(&a.intersect(b)).max(0.)
}

/// What to keep of a window: GPUI's restore rectangle (the normal one while maximized), and
/// whether it was maximized. Fullscreen is not a state this app offers, so it keeps the rectangle
/// only.
pub fn to_saved(bounds: WindowBounds, display: Option<String>) -> SavedWindow {
    let (rect, maximized) = match bounds {
        WindowBounds::Windowed(b) | WindowBounds::Fullscreen(b) => (b, false),
        WindowBounds::Maximized(b) => (b, true),
    };
    let whole = |p: Pixels| f32::from(p).round();
    SavedWindow {
        x: whole(rect.origin.x) as i32,
        y: whole(rect.origin.y) as i32,
        width: whole(rect.size.width).max(0.) as u32,
        height: whole(rect.size.height).max(0.) as u32,
        maximized,
        display,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: &str = "11111111-1111-4111-8111-111111111111";
    const SECOND: &str = "22222222-2222-4222-8222-222222222222";

    fn screen(uuid: &str, x: f32, y: f32, w: f32, h: f32) -> Screen {
        Screen {
            area: Bounds::new(point(px(x), px(y)), size(px(w), px(h))),
            uuid: Some(uuid.into()),
        }
    }

    fn one() -> Vec<Screen> {
        vec![screen(PRIMARY, 0., 0., 1920., 1040.)]
    }

    fn two() -> Vec<Screen> {
        let mut screens = one();
        screens.push(screen(SECOND, 1920., 0., 1280., 1024.));
        screens
    }

    fn at(x: i32, y: i32, width: u32, height: u32, display: &str) -> SavedWindow {
        SavedWindow {
            x,
            y,
            width,
            height,
            maximized: false,
            display: Some(display.into()),
        }
    }

    #[test]
    fn a_window_on_a_display_opens_where_it_was() {
        let saved = at(300, 200, 800, 600, PRIMARY);
        let p = place(Some(&saved), &one());
        assert_eq!(p.origin, Some(point(px(300.), px(200.))));
        assert_eq!(p.size, size(px(800.), px(600.)));
        assert_eq!((p.screen, p.maximized), (Some(0), false));
    }

    #[test]
    fn a_maximized_window_opens_maximized_over_its_restore_rectangle() {
        let saved = SavedWindow {
            maximized: true,
            ..at(2100, 100, 800, 600, SECOND)
        };
        let p = place(Some(&saved), &two());
        assert!(p.maximized);
        assert_eq!(p.size, size(px(800.), px(600.)));
        assert_eq!(p.screen, Some(1));
    }

    #[test]
    fn nothing_saved_is_the_default_centered() {
        assert_eq!(place(None, &one()), Placement::centered(None));
        assert_eq!(place(None, &one()).size, size(px(680.), px(480.)));
    }

    #[test]
    fn a_window_off_every_display_falls_back_to_the_default() {
        let saved = at(-4000, 100, 800, 600, PRIMARY);
        let p = place(Some(&saved), &one());
        assert_eq!(
            p,
            Placement::centered(Some(0)),
            "centered on the display it was on"
        );
        assert_eq!(p.size, size(px(680.), px(480.)));
    }

    #[test]
    fn a_window_mostly_off_a_display_falls_back_to_the_default() {
        let saved = at(1700, 500, 800, 600, PRIMARY);
        assert_eq!(place(Some(&saved), &one()).origin, None);
        let barely_on = at(1920 - 500, 100, 800, 600, PRIMARY);
        assert!(place(Some(&barely_on), &one()).origin.is_some(), "62% on");
    }

    #[test]
    fn an_unplugged_second_display_sends_the_window_to_the_default() {
        let saved = at(2200, 150, 800, 600, SECOND);
        assert_eq!(place(Some(&saved), &two()).screen, Some(1));
        let p = place(Some(&saved), &one());
        assert_eq!(p, Placement::centered(None));
    }

    #[test]
    fn a_layout_that_moved_keeps_the_window_on_whichever_display_holds_it() {
        let saved = at(2200, 150, 800, 600, PRIMARY);
        let p = place(Some(&saved), &two());
        assert_eq!(p.screen, Some(1));
        assert_eq!(p.origin, Some(point(px(2200.), px(150.))));
    }

    #[test]
    fn a_window_smaller_than_the_small_one_grows_to_it() {
        let saved = at(100, 100, 120, 90, PRIMARY);
        let p = place(Some(&saved), &one());
        assert_eq!(p.size, size(px(480.), px(400.)));
        assert_eq!(p.origin, Some(point(px(100.), px(100.))));
    }

    #[test]
    fn a_window_larger_than_its_display_shrinks_to_it() {
        let saved = at(0, 0, 2200, 1200, PRIMARY);
        let p = place(Some(&saved), &one());
        assert_eq!(p.size, size(px(1920.), px(1040.)));
        let tiny = vec![screen(PRIMARY, 0., 0., 400., 300.)];
        let p = place(Some(&at(0, 0, 500, 420, PRIMARY)), &tiny);
        assert_eq!(p.size, size(px(480.), px(400.)), "never below the minimum");
    }

    #[test]
    fn what_is_saved_is_the_restore_rectangle() {
        let rect = Bounds::new(point(px(10.4), px(-20.6)), size(px(800.4), px(600.)));
        let display = Some(PRIMARY.to_owned());
        let saved = to_saved(WindowBounds::Maximized(rect), display.clone());
        assert_eq!(
            saved,
            SavedWindow {
                maximized: true,
                ..at(10, -21, 800, 600, PRIMARY)
            }
        );
        assert!(!to_saved(WindowBounds::Windowed(rect), display.clone()).maximized);
        assert!(!to_saved(WindowBounds::Fullscreen(rect), display).maximized);
    }
}
