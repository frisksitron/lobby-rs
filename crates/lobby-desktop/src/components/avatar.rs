//! Built on the kit's unstyled `base::Avatar`: `component::Avatar` sizes the initials' box
//! instead of their text, leaving them in the top-left of a half-size box.

use gpui_kit::{
    App, FontWeight, Hsla, ImageSource, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window,
    base::{Avatar as BaseAvatar, AvatarFallback, AvatarImage},
    component::{ActiveTheme as _, oklch},
    div,
    prelude::FluentBuilder as _,
    relative,
};

use super::tokens::{AvatarSize, mark_on_dark};
use crate::scale::u;

const STOP_WORDS: [&str; 5] = ["the", "a", "an", "our", "my"];
pub(super) const IDENTITY_HUES: usize = 12;

pub fn initials(name: &str) -> SharedString {
    let words: Vec<&str> = name.split_whitespace().collect();
    let skip = words
        .iter()
        .take_while(|w| STOP_WORDS.contains(&w.to_lowercase().as_str()))
        .count();
    // A name made only of stop-words keeps them.
    let words = if skip < words.len() {
        &words[skip..]
    } else {
        &words[..]
    };
    let initials: String = words
        .iter()
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if initials.is_empty() {
        "L".into()
    } else {
        initials.into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Paint {
    Identity,
    Primary,
    Fill(Hsla),
}

#[derive(IntoElement)]
pub struct Avatar {
    name: SharedString,
    size: AvatarSize,
    paint: Paint,
    image: Option<ImageSource>,
    selector: Option<SharedString>,
}

impl Avatar {
    pub fn member(name: impl Into<SharedString>) -> Self {
        Self::new(name.into(), Paint::Identity)
    }

    pub fn lobby(name: impl Into<SharedString>, color: Hsla) -> Self {
        Self::new(name.into(), Paint::Fill(color))
    }

    pub fn app() -> Self {
        Self::new("Lobby".into(), Paint::Primary)
    }

    fn new(name: SharedString, paint: Paint) -> Self {
        Self {
            name,
            size: AvatarSize::Row,
            paint,
            image: None,
            selector: None,
        }
    }

    pub fn size(mut self, size: AvatarSize) -> Self {
        self.size = size;
        self
    }

    pub fn image(mut self, image: Option<ImageSource>) -> Self {
        self.image = image;
        self
    }

    pub fn selector(mut self, id: impl Into<SharedString>) -> Self {
        self.selector = Some(id.into());
        self
    }
}

impl RenderOnce for Avatar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let side = self.size.px();
        let theme = cx.theme();
        let (bg, fg, border) = match self.paint {
            Paint::Identity => identity_colors(&self.name),
            Paint::Primary => (theme.primary, theme.primary_foreground, theme.primary),
            Paint::Fill(color) => {
                let color = mark_on_dark(color);
                (color, gpui_kit::white(), color)
            }
        };
        let circle = self.paint == Paint::Identity;
        // A Lobby's own picture is not framed in the colour of its letter mark.
        let border = if self.image.is_some() && !circle && self.paint != Paint::Primary {
            gpui_kit::transparent_black()
        } else {
            border
        };
        let radius = if circle { side / 2. } else { side * 0.28 };
        let text = initials(&self.name);
        // Two letters need a smaller size to sit inside the same box.
        let scale = if text.chars().count() > 1 { 0.42 } else { 0.5 };
        let selector = self.selector;
        let label = div()
            .when_some(selector.clone(), |d, id| {
                d.debug_selector(move || format!("{id}-initials"))
            })
            .text_size(u((side * scale).round()))
            .line_height(relative(1.))
            .whitespace_nowrap()
            .font_weight(FontWeight::SEMIBOLD)
            .child(text);
        BaseAvatar::new()
            .when_some(selector, |d, id| d.debug_selector(move || id.to_string()))
            .size(u(side))
            .flex_shrink_0()
            .rounded(u(radius))
            .border_1()
            .border_color(border)
            // GPUI clips to rectangles only, so each layer rounds itself.
            .when_some(self.image, |a, src| {
                a.image(AvatarImage::new(src).size_full().rounded(u(radius)))
            })
            .fallback(
                AvatarFallback::new()
                    .size_full()
                    .rounded(u(radius))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(bg)
                    .text_color(fg)
                    .child(label),
            )
    }
}

/// FNV-1a with a 64-bit finalizer, so names that differ in one character land on unrelated colours.
/// Stable across builds.
pub fn stable_hash(text: &str) -> u64 {
    let mut h = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

pub fn identity_slot(name: &str) -> usize {
    (stable_hash(name) % IDENTITY_HUES as u64) as usize
}

fn identity_colors(name: &str) -> (Hsla, Hsla, Hsla) {
    let hue = identity_slot(name) as f32 * (360. / IDENTITY_HUES as f32);
    (
        oklch(0.42, 0.07, hue),
        oklch(0.93, 0.05, hue),
        oklch(0.48, 0.08, hue),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{Context, Render, TestAppContext, VisualTestContext, px};

    struct Sheet;

    impl Render for Sheet {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(AvatarSize::ALL.into_iter().flat_map(|size| {
                    let key = size.px();
                    [
                        Avatar::member("alice")
                            .size(size)
                            .selector(format!("one-{key}")),
                        Avatar::member("Mary Jane")
                            .size(size)
                            .selector(format!("two-{key}")),
                        Avatar::lobby("Dev Lobby", gpui_kit::black())
                            .size(size)
                            .selector(format!("mark-{key}")),
                    ]
                }))
        }
    }

    #[gpui_kit::test]
    fn the_initials_sit_in_the_middle_at_every_size(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| Sheet);
        let cx: &mut VisualTestContext = cx;
        cx.update(|window, cx| window.draw(cx).clear(cx));
        for size in AvatarSize::ALL {
            for kind in ["one", "two", "mark"] {
                let id = format!("{kind}-{}", size.px());
                let avatar = cx.debug_bounds(id.clone().leak()).expect("the avatar");
                let text = cx
                    .debug_bounds(format!("{id}-initials").leak())
                    .expect("its initials");
                assert_eq!(avatar.size.width, px(size.px()), "{id}");
                let (a, t) = (avatar.center(), text.center());
                assert!(
                    (a.x - t.x).abs() <= px(0.5) && (a.y - t.y).abs() <= px(0.5),
                    "{id}: initials centred at {t:?}, the avatar at {a:?}"
                );
            }
        }
    }
}
