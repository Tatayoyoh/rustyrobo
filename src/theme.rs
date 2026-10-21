//! Colour themes. Each theme defines the full UI palette, the JavaScript
//! syntax colours of the editor and the colours of BSON values in results.

use egui::{Color32, CornerRadius, Shadow, Stroke, Visuals, style::WidgetVisuals};

pub const DEFAULT_THEME: &str = "System";

#[derive(Debug, Clone, Copy)]
pub struct Syntax {
    pub keyword: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
    pub function: Color32,
    pub property: Color32,
    pub punctuation: Color32,
    pub regex: Color32,
    pub constant: Color32,
    /// BSON constructors: ObjectId, ISODate, NumberLong…
    pub bson_type: Color32,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub dark: bool,
    /// Window and panel background.
    pub bg: Color32,
    /// Editors, tables, text fields.
    pub base: Color32,
    /// Buttons and inactive widgets.
    pub surface: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub accent: Color32,
    /// Text drawn on top of `accent`.
    pub on_accent: Color32,
    pub selection: Color32,
    pub stripe: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,
    pub syntax: Syntax,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "Porcelain",
        dark: false,
        bg: hex(0xF3F4F6),
        base: hex(0xFFFFFF),
        surface: hex(0xE6E8EC),
        surface_hover: hex(0xD9DCE2),
        border: hex(0xC9CDD4),
        text: hex(0x1F2328),
        text_weak: hex(0x656D76),
        accent: hex(0x2563EB),
        on_accent: hex(0xFFFFFF),
        selection: hex(0xC7DBFF),
        stripe: hex(0xF7F8FA),
        error: hex(0xCF222E),
        warning: hex(0x9A6700),
        success: hex(0x1A7F37),
        syntax: Syntax {
            keyword: hex(0xA626A4),
            string: hex(0x0A7B3E),
            number: hex(0xB35900),
            comment: hex(0x8C959F),
            function: hex(0x2563EB),
            property: hex(0x953800),
            punctuation: hex(0x57606A),
            regex: hex(0xC2185B),
            constant: hex(0x0550AE),
            bson_type: hex(0x8250DF),
        },
    },
    Theme {
        name: "Graphite",
        dark: true,
        bg: hex(0x1E1F22),
        base: hex(0x17181A),
        surface: hex(0x2B2D31),
        surface_hover: hex(0x383A40),
        border: hex(0x3A3D43),
        text: hex(0xDCDFE4),
        text_weak: hex(0x8B9099),
        accent: hex(0x2DB8A6),
        on_accent: hex(0x0B1F1C),
        selection: hex(0x1F4E49),
        stripe: hex(0x1B1C1F),
        error: hex(0xF2706B),
        warning: hex(0xE5B567),
        success: hex(0x7BC67B),
        syntax: Syntax {
            keyword: hex(0xC792EA),
            string: hex(0x9CCC65),
            number: hex(0xF4A35A),
            comment: hex(0x6B7079),
            function: hex(0x6CB6FF),
            property: hex(0x56D6C6),
            punctuation: hex(0xA6ACB5),
            regex: hex(0xF07178),
            constant: hex(0xFF9CAC),
            bson_type: hex(0xE7C66B),
        },
    },
    Theme {
        name: "Nord",
        dark: true,
        bg: hex(0x2E3440),
        base: hex(0x272C36),
        surface: hex(0x3B4252),
        surface_hover: hex(0x434C5E),
        border: hex(0x4C566A),
        text: hex(0xECEFF4),
        text_weak: hex(0x9AA5B8),
        accent: hex(0x88C0D0),
        on_accent: hex(0x2E3440),
        selection: hex(0x40576B),
        stripe: hex(0x2A303B),
        error: hex(0xBF616A),
        warning: hex(0xEBCB8B),
        success: hex(0xA3BE8C),
        syntax: Syntax {
            keyword: hex(0x81A1C1),
            string: hex(0xA3BE8C),
            number: hex(0xB48EAD),
            comment: hex(0x616E88),
            function: hex(0x88C0D0),
            property: hex(0x8FBCBB),
            punctuation: hex(0xD8DEE9),
            regex: hex(0xEBCB8B),
            constant: hex(0x5E81AC),
            bson_type: hex(0xD08770),
        },
    },
    Theme {
        name: "Solarized Light",
        dark: false,
        bg: hex(0xEEE8D5),
        base: hex(0xFDF6E3),
        surface: hex(0xE4DDC8),
        surface_hover: hex(0xD9D2BC),
        border: hex(0xC9C2AC),
        text: hex(0x586E75),
        text_weak: hex(0x839496),
        accent: hex(0x268BD2),
        on_accent: hex(0xFDF6E3),
        selection: hex(0xD3E3EC),
        stripe: hex(0xF7F0DC),
        error: hex(0xDC322F),
        warning: hex(0xB58900),
        success: hex(0x859900),
        syntax: Syntax {
            keyword: hex(0x859900),
            string: hex(0x2AA198),
            number: hex(0xD33682),
            comment: hex(0x93A1A1),
            function: hex(0x268BD2),
            property: hex(0xB58900),
            punctuation: hex(0x657B83),
            regex: hex(0xDC322F),
            constant: hex(0xCB4B16),
            bson_type: hex(0x6C71C4),
        },
    },
    Theme {
        name: "Solarized Dark",
        dark: true,
        bg: hex(0x073642),
        base: hex(0x002B36),
        surface: hex(0x0E4452),
        surface_hover: hex(0x155263),
        border: hex(0x24606F),
        text: hex(0x93A1A1),
        text_weak: hex(0x657B83),
        accent: hex(0x2AA198),
        on_accent: hex(0x002B36),
        selection: hex(0x184E5A),
        stripe: hex(0x01313D),
        error: hex(0xDC322F),
        warning: hex(0xB58900),
        success: hex(0x859900),
        syntax: Syntax {
            keyword: hex(0x859900),
            string: hex(0x2AA198),
            number: hex(0xD33682),
            comment: hex(0x586E75),
            function: hex(0x268BD2),
            property: hex(0xB58900),
            punctuation: hex(0x839496),
            regex: hex(0xDC322F),
            constant: hex(0xCB4B16),
            bson_type: hex(0x6C71C4),
        },
    },
    Theme {
        name: "Gruvbox Dark",
        dark: true,
        bg: hex(0x282828),
        base: hex(0x1D2021),
        surface: hex(0x3C3836),
        surface_hover: hex(0x504945),
        border: hex(0x504945),
        text: hex(0xEBDBB2),
        text_weak: hex(0xA89984),
        accent: hex(0xFABD2F),
        on_accent: hex(0x282828),
        selection: hex(0x4F4632),
        stripe: hex(0x242424),
        error: hex(0xFB4934),
        warning: hex(0xFE8019),
        success: hex(0xB8BB26),
        syntax: Syntax {
            keyword: hex(0xFB4934),
            string: hex(0xB8BB26),
            number: hex(0xD3869B),
            comment: hex(0x928374),
            function: hex(0xFABD2F),
            property: hex(0x83A598),
            punctuation: hex(0xD5C4A1),
            regex: hex(0xFE8019),
            constant: hex(0xD3869B),
            bson_type: hex(0x8EC07C),
        },
    },
    Theme {
        name: "One Dark",
        dark: true,
        bg: hex(0x21252B),
        base: hex(0x282C34),
        surface: hex(0x2C313A),
        surface_hover: hex(0x3A3F4B),
        border: hex(0x3E4451),
        text: hex(0xABB2BF),
        text_weak: hex(0x7F848E),
        accent: hex(0x61AFEF),
        on_accent: hex(0x21252B),
        selection: hex(0x3E4F6A),
        stripe: hex(0x252930),
        error: hex(0xE06C75),
        warning: hex(0xE5C07B),
        success: hex(0x98C379),
        syntax: Syntax {
            keyword: hex(0xC678DD),
            string: hex(0x98C379),
            number: hex(0xD19A66),
            comment: hex(0x5C6370),
            function: hex(0x61AFEF),
            property: hex(0xE06C75),
            punctuation: hex(0xABB2BF),
            regex: hex(0x56B6C2),
            constant: hex(0xD19A66),
            bson_type: hex(0xE5C07B),
        },
    },
    Theme {
        name: "Dracula",
        dark: true,
        bg: hex(0x21222C),
        base: hex(0x282A36),
        surface: hex(0x343746),
        surface_hover: hex(0x44475A),
        border: hex(0x44475A),
        text: hex(0xF8F8F2),
        text_weak: hex(0x9EA8C7),
        accent: hex(0xBD93F9),
        on_accent: hex(0x21222C),
        selection: hex(0x4A3F6B),
        stripe: hex(0x252631),
        error: hex(0xFF5555),
        warning: hex(0xF1FA8C),
        success: hex(0x50FA7B),
        syntax: Syntax {
            keyword: hex(0xFF79C6),
            string: hex(0xF1FA8C),
            number: hex(0xBD93F9),
            comment: hex(0x6272A4),
            function: hex(0x50FA7B),
            property: hex(0x8BE9FD),
            punctuation: hex(0xF8F8F2),
            regex: hex(0xFF5555),
            constant: hex(0xBD93F9),
            bson_type: hex(0xFFB86C),
        },
    },
    Theme {
        name: "Tokyo Night",
        dark: true,
        bg: hex(0x16161E),
        base: hex(0x1A1B26),
        surface: hex(0x24283B),
        surface_hover: hex(0x2F3549),
        border: hex(0x2F3549),
        text: hex(0xC0CAF5),
        text_weak: hex(0x7A82A6),
        accent: hex(0x7AA2F7),
        on_accent: hex(0x16161E),
        selection: hex(0x2E3C64),
        stripe: hex(0x181923),
        error: hex(0xF7768E),
        warning: hex(0xE0AF68),
        success: hex(0x9ECE6A),
        syntax: Syntax {
            keyword: hex(0xBB9AF7),
            string: hex(0x9ECE6A),
            number: hex(0xFF9E64),
            comment: hex(0x565F89),
            function: hex(0x7AA2F7),
            property: hex(0x73DACA),
            punctuation: hex(0x89DDFF),
            regex: hex(0xB4F9F8),
            constant: hex(0xFF9E64),
            bson_type: hex(0x2AC3DE),
        },
    },
    Theme {
        name: "Catppuccin Latte",
        dark: false,
        bg: hex(0xE6E9EF),
        base: hex(0xEFF1F5),
        surface: hex(0xDCE0E8),
        surface_hover: hex(0xCCD0DA),
        border: hex(0xBCC0CC),
        text: hex(0x4C4F69),
        text_weak: hex(0x6C6F85),
        accent: hex(0x8839EF),
        on_accent: hex(0xEFF1F5),
        selection: hex(0xD9CCF6),
        stripe: hex(0xE9ECF2),
        error: hex(0xD20F39),
        warning: hex(0xDF8E1D),
        success: hex(0x40A02B),
        syntax: Syntax {
            keyword: hex(0x8839EF),
            string: hex(0x40A02B),
            number: hex(0xFE640B),
            comment: hex(0x9CA0B0),
            function: hex(0x1E66F5),
            property: hex(0x179299),
            punctuation: hex(0x5C5F77),
            regex: hex(0xEA76CB),
            constant: hex(0xFE640B),
            bson_type: hex(0xD20F39),
        },
    },
    Theme {
        name: "Catppuccin Mocha",
        dark: true,
        bg: hex(0x181825),
        base: hex(0x1E1E2E),
        surface: hex(0x313244),
        surface_hover: hex(0x45475A),
        border: hex(0x45475A),
        text: hex(0xCDD6F4),
        text_weak: hex(0x9399B2),
        accent: hex(0xCBA6F7),
        on_accent: hex(0x1E1E2E),
        selection: hex(0x45405E),
        stripe: hex(0x1B1B29),
        error: hex(0xF38BA8),
        warning: hex(0xFAB387),
        success: hex(0xA6E3A1),
        syntax: Syntax {
            keyword: hex(0xCBA6F7),
            string: hex(0xA6E3A1),
            number: hex(0xFAB387),
            comment: hex(0x6C7086),
            function: hex(0x89B4FA),
            property: hex(0x94E2D5),
            punctuation: hex(0xBAC2DE),
            regex: hex(0xF5C2E7),
            constant: hex(0xFAB387),
            bson_type: hex(0xF9E2AF),
        },
    },
    Theme {
        name: "High Contrast",
        dark: true,
        bg: hex(0x000000),
        base: hex(0x000000),
        surface: hex(0x1A1A1A),
        surface_hover: hex(0x333333),
        border: hex(0xFFFFFF),
        text: hex(0xFFFFFF),
        text_weak: hex(0xD0D0D0),
        accent: hex(0xFFD400),
        on_accent: hex(0x000000),
        selection: hex(0x0050A0),
        stripe: hex(0x0D0D0D),
        error: hex(0xFF4D4D),
        warning: hex(0xFFD400),
        success: hex(0x3DFF6E),
        syntax: Syntax {
            keyword: hex(0x6FC3FF),
            string: hex(0x9BFF8A),
            number: hex(0xFFB15E),
            comment: hex(0xB0B0B0),
            function: hex(0xFFE36E),
            property: hex(0x7FFFE8),
            punctuation: hex(0xFFFFFF),
            regex: hex(0xFF8AD8),
            constant: hex(0xFFB15E),
            bson_type: hex(0xFF8AD8),
        },
    },
];

/// Theme names offered in View → Theme. "System" follows the OS preference.
pub fn names() -> impl Iterator<Item = &'static str> {
    std::iter::once("System").chain(THEMES.iter().map(|t| t.name))
}

pub fn resolve(name: &str, system_dark: bool) -> &'static Theme {
    if name == "System" {
        return if system_dark { &THEMES[1] } else { &THEMES[0] };
    }
    THEMES.iter().find(|t| t.name == name).unwrap_or(&THEMES[0])
}

fn widget(bg: Color32, stroke: Color32, fg: Color32, radius: u8) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, stroke),
        corner_radius: CornerRadius::same(radius),
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    }
}

pub fn visuals(t: &Theme) -> Visuals {
    let mut v = if t.dark { Visuals::dark() } else { Visuals::light() };
    v.dark_mode = t.dark;
    v.override_text_color = None;
    v.hyperlink_color = t.accent;
    v.faint_bg_color = t.stripe;
    v.extreme_bg_color = t.base;
    v.text_edit_bg_color = Some(t.base);
    v.code_bg_color = t.surface;
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.error;
    v.window_fill = t.bg;
    v.panel_fill = t.bg;
    v.window_stroke = Stroke::new(1.0, t.border);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    let shadow = if t.dark { 110 } else { 40 };
    v.window_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(shadow) };
    v.popup_shadow = Shadow { offset: [0, 3], blur: 10, spread: 0, color: Color32::from_black_alpha(shadow) };
    v.selection.bg_fill = t.selection;
    v.selection.stroke = Stroke::new(1.0, t.text);
    v.striped = true;

    v.widgets.noninteractive = widget(t.bg, t.border, t.text, 4);
    v.widgets.inactive = widget(t.surface, t.border, t.text, 4);
    v.widgets.hovered = widget(t.surface_hover, t.accent, t.text, 4);
    // egui derives the "strong" text colour from the active widget foreground.
    v.widgets.active = widget(t.selection, t.accent, t.text, 4);
    v.widgets.open = widget(t.surface_hover, t.border, t.text, 4);
    v.text_cursor.stroke = Stroke::new(2.0, t.accent);
    v
}

pub fn apply(ctx: &egui::Context, t: &Theme, font_size: f32) {
    let vis = visuals(t);
    ctx.set_visuals_of(egui::Theme::Dark, vis.clone());
    ctx.set_visuals_of(egui::Theme::Light, vis);
    ctx.set_theme(if t.dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.all_styles_mut(|s| {
        use egui::{FontFamily, FontId, TextStyle};
        s.text_styles = [
            (TextStyle::Small, FontId::new(font_size - 2.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(font_size, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(font_size, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(font_size + 5.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(font_size, FontFamily::Monospace)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size.y = font_size + 9.0;
    });
}

/// Colour of a BSON value in tree/table/text views.
pub fn bson_color(t: &Theme, b: &bson::Bson) -> Color32 {
    use bson::Bson::*;
    match b {
        String(_) | Symbol(_) => t.syntax.string,
        Double(_) | Int32(_) | Int64(_) | Decimal128(_) => t.syntax.number,
        Boolean(_) | Null | Undefined | MinKey | MaxKey => t.syntax.constant,
        ObjectId(_) | DateTime(_) | Binary(_) | Timestamp(_) | DbPointer(_) => t.syntax.bson_type,
        RegularExpression(_) => t.syntax.regex,
        JavaScriptCode(_) | JavaScriptCodeWithScope(_) => t.syntax.function,
        Document(_) | Array(_) => t.text_weak,
    }
}
