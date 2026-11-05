// hallmark v2.0.0 · theme: custom (by request: minimal black and white, after Wispr Flow) · genre: atmospheric · scope: app screens · signature: the black recording pill whose white bars move with the voice · inferred: material = the existing app, audience = anyone dictating into another app, use = record and paste, tone = minimal monochrome
// critique: ink on paper and nothing else; the one filled control on a screen is the primary action, selection is a grey fill, and the pill is black in either appearance. One family, Figtree, carries every role.
//! Every colour and font the app draws with. Light is black on white, dark
//! is white on black; the pill is always black.
use gpui::{App, BoxShadow, Global, Hsla, Pixels, SharedString, WindowAppearance, point, px, rgb};
use gpui_component::{Theme, ThemeMode};

pub const BODY: &str = "Figtree";

pub const RADIUS_CARD: Pixels = px(14.);
pub const RADIUS_PART: Pixels = px(8.);

pub const TEXT: Pixels = px(15.);
pub const LABEL: Pixels = px(13.);
pub const TITLE: Pixels = px(20.);

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub dark: bool,
    pub paper: Hsla,
    pub paper_2: Hsla,
    pub paper_3: Hsla,
    pub ink: Hsla,
    pub ink_2: Hsla,
    pub muted: Hsla,
    pub rule: Hsla,
    pub focus: Hsla,
    pub shadow: Hsla,
}

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

impl Tokens {
    pub fn light() -> Self {
        Self {
            dark: false,
            paper: hex(0xffffff),
            paper_2: hex(0xf6f6f6),
            paper_3: hex(0xededed),
            ink: hex(0x0a0a0a),
            ink_2: hex(0x404040),
            muted: hex(0x737373),
            rule: hex(0xe6e6e6),
            focus: hex(0x0a0a0a).opacity(0.45),
            shadow: hex(0x000000).opacity(0.12),
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            paper: hex(0x0a0a0a),
            paper_2: hex(0x141414),
            paper_3: hex(0x212121),
            ink: hex(0xfafafa),
            ink_2: hex(0xd4d4d4),
            muted: hex(0x8c8c8c),
            rule: hex(0x262626),
            focus: hex(0xfafafa).opacity(0.5),
            shadow: hex(0x000000).opacity(0.5),
        }
    }

    /// The pill: black in either appearance.
    pub fn pill() -> Self {
        Self {
            dark: true,
            paper: hex(0x000000),
            paper_2: hex(0x161616),
            paper_3: hex(0x2a2a2a),
            ink: hex(0xffffff),
            ink_2: hex(0xe6e6e6),
            muted: hex(0xffffff).opacity(0.55),
            rule: hex(0xffffff).opacity(0.14),
            focus: hex(0xffffff).opacity(0.55),
            shadow: hex(0x000000).opacity(0.35),
        }
    }

    /// A floating surface: the pill and toasts.
    pub fn float_shadow(&self) -> Vec<BoxShadow> {
        vec![BoxShadow {
            color: self.shadow,
            offset: point(px(0.), px(6.)),
            blur_radius: px(20.),
            spread_radius: px(-4.),
            inset: false,
        }]
    }

    pub fn ring(&self) -> Vec<BoxShadow> {
        vec![BoxShadow {
            color: self.focus,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(2.),
            inset: false,
        }]
    }
}

pub struct Monochrome(pub Tokens);
impl Global for Monochrome {}

pub fn tokens(cx: &App) -> Tokens {
    cx.try_global::<Monochrome>()
        .map(|theme| theme.0)
        .unwrap_or_else(Tokens::light)
}

/// Loads the face and applies the appearance.
pub fn init(appearance: WindowAppearance, cx: &mut App) {
    let fonts = vec![
        include_bytes!("../assets/fonts/Figtree-Regular.ttf")
            .as_slice()
            .into(),
        include_bytes!("../assets/fonts/Figtree-Medium.ttf")
            .as_slice()
            .into(),
        include_bytes!("../assets/fonts/Figtree-SemiBold.ttf")
            .as_slice()
            .into(),
    ];
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        eprintln!("Fonts unavailable: {error}");
    }
    apply(appearance, cx);
}

pub fn apply(appearance: WindowAppearance, cx: &mut App) {
    let dark = matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    if cx
        .try_global::<Monochrome>()
        .is_some_and(|theme| theme.0.dark == dark)
    {
        return;
    }
    let t = if dark {
        Tokens::dark()
    } else {
        Tokens::light()
    };
    cx.set_global(Monochrome(t));
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    Theme::update(cx, |theme| {
        theme.font_family = SharedString::new_static(BODY);
        theme.font_size = TEXT;
        theme.radius = RADIUS_PART;
        theme.radius_lg = RADIUS_CARD;
        theme.shadow = false;
        let c = &mut theme.colors;
        c.background = t.paper;
        c.foreground = t.ink;
        c.border = t.rule;
        c.input = t.rule;
        c.ring = t.focus;
        c.caret = t.ink;
        c.selection = t.ink.opacity(0.18);
        c.muted = t.paper_2;
        c.muted_foreground = t.muted;
        c.accent = t.paper_3;
        c.accent_foreground = t.ink;
        c.popover = t.paper;
        c.popover_foreground = t.ink;
        c.primary = t.ink;
        c.primary_hover = t.ink_2;
        c.primary_active = t.ink_2;
        c.primary_foreground = t.paper;
        c.secondary = t.paper_2;
        c.secondary_hover = t.paper_3;
        c.secondary_active = t.rule;
        c.secondary_foreground = t.ink;
        c.danger = t.ink;
        c.danger_hover = t.ink_2;
        c.danger_active = t.ink_2;
        c.danger_foreground = t.paper;
        c.list = t.paper;
        c.list_hover = t.paper_2;
        c.list_active = t.paper_3;
        c.list_active_border = t.rule;
        c.list_even = t.paper;
        c.list_head = t.paper;
        c.switch = t.rule;
        c.switch_thumb = t.paper;
        c.title_bar = t.paper;
        c.title_bar_border = t.rule;
        c.scrollbar = t.paper.opacity(0.);
        c.scrollbar_thumb = t.rule;
        c.scrollbar_thumb_hover = t.muted;
        c.progress_bar = t.ink;
        c.skeleton = t.paper_3;
        c.link = t.ink;
        c.link_hover = t.ink_2;
        c.link_active = t.ink_2;
    });
}
