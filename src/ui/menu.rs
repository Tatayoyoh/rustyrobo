//! Entries of the context menus: an icon for the major actions, and the
//! key word of the action in the colour of its kind (create, edit, delete).

use egui::text::LayoutJob;
use egui::{AtomExt, Color32, FontSelection, Id, Response, RichText, Ui, WidgetInfo, WidgetType};

use crate::theme::Theme;
use crate::ui::icon;

/// What an action does, which colours its key word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Create,
    Edit,
    Delete,
}

/// Kind of a word of a label, if it is a key word.
fn kind(word: &str) -> Option<Kind> {
    match word.to_ascii_lowercase().as_str() {
        "create" | "add" | "insert" | "new" => Some(Kind::Create),
        "edit" | "update" => Some(Kind::Edit),
        "remove" | "delete" | "drop" => Some(Kind::Delete),
        _ => None,
    }
}

/// Icon of an action, from the verb starting its label. Actions whose verb
/// is coloured have none: the colour tells them apart already.
pub fn icon_of(label: &str) -> Option<&'static str> {
    let verb = label.split_whitespace().next()?.to_ascii_lowercase();
    Some(match verb.as_str() {
        _ if kind(&verb).is_some() => return None,
        "rename" => icon::PENCIL_SIMPLE,
        "copy" | "duplicate" => icon::COPY,
        "view" => icon::EYE,
        "open" => icon::TERMINAL_WINDOW,
        "load" => icon::FOLDER_OPEN,
        "refresh" | "reconnect" | "re-execute" => icon::ARROWS_CLOCKWISE,
        "import" => icon::DOWNLOAD_SIMPLE,
        "export" => icon::EXPORT,
        "dump" => icon::ARCHIVE,
        "restore" => icon::CLOCK_COUNTER_CLOCKWISE,
        "connect" => icon::PLUG,
        "disconnect" => icon::PLUGS,
        "close" => icon::X,
        "expand" => icon::ARROWS_OUT_SIMPLE,
        "collapse" => icon::ARROWS_IN_SIMPLE,
        "repair" => icon::WRENCH,
        _ if label.contains("Statistics") => icon::CHART_BAR,
        _ => return None,
    })
}

/// The verb starting a label, with its kind if it is a key word, and the rest.
pub fn split(label: &str) -> (Option<(&str, Kind)>, &str) {
    let first = label.split(' ').next().unwrap_or_default().trim_end_matches(['…', ',', '.']);
    match kind(first) {
        Some(k) => (Some((first, k)), &label[first.len()..]),
        None => (None, label),
    }
}

fn color(t: &Theme, k: Kind) -> Color32 {
    match k {
        Kind::Create => t.accent,
        Kind::Edit => t.secondary,
        Kind::Delete => t.error,
    }
}

/// Where `theme::apply` keeps the theme for the menus.
pub fn theme_id() -> Id {
    Id::new("menu-theme")
}

/// An entry of a context menu. Screen readers (and the tests) read the
/// label and the shortcut, without the icon, as for a plain button.
pub fn entry(ui: &mut Ui, enabled: bool, label: &str, shortcut: &str) -> Response {
    let theme = ui.ctx().data(|d| d.get_temp::<Theme>(theme_id())).unwrap_or(crate::theme::THEMES[0]);
    let mut job = LayoutJob::default();
    let style = ui.style().clone();
    let (key, after) = split(label);
    if let Some((word, k)) = key {
        // Disabled, the word fades with the rest.
        let word = if enabled { RichText::new(word).color(color(&theme, k)) } else { RichText::new(word) };
        word.append_to(&mut job, &style, FontSelection::Default, egui::Align::Center);
    }
    RichText::new(after).append_to(&mut job, &style, FontSelection::Default, egui::Align::Center);
    let size = egui::Vec2::splat(ui.text_style_height(&egui::TextStyle::Button));
    let mut b = egui::Button::new((icon_of(label).unwrap_or("").atom_size(size), job));
    if !shortcut.is_empty() {
        b = b.shortcut_text(shortcut);
    }
    let r = ui.add_enabled(enabled, b);
    let read = if shortcut.is_empty() { label.to_string() } else { format!("{label} {shortcut}") };
    r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &read));
    r
}

/// [`entry`], enabled, without shortcut.
pub fn button(ui: &mut Ui, label: &str) -> Response {
    entry(ui, true, label, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_words_are_coloured_alone() {
        assert_eq!(split("Create Collection…"), (Some(("Create", Kind::Create)), " Collection…"));
        assert_eq!(split("Edit…"), (Some(("Edit", Kind::Edit)), "…"));
        assert_eq!(split("Delete 3 Documents…"), (Some(("Delete", Kind::Delete)), " 3 Documents…"));
        assert_eq!(split("Duplicate Query In New Tab"), (None, "Duplicate Query In New Tab"), "the verb only");
        assert_eq!(split("Copy JSON"), (None, "Copy JSON"));
        assert_eq!(split("Remove Documents…").0, Some(("Remove", Kind::Delete)));
        assert_eq!(split("Drop Database…").0, Some(("Drop", Kind::Delete)));
        assert_eq!(split("Insert Document…").0, Some(("Insert", Kind::Create)));
    }

    #[test]
    fn major_actions_have_an_icon() {
        assert_eq!(icon_of("Rename Collection…"), Some(icon::PENCIL_SIMPLE));
        assert_eq!(icon_of("Database Statistics"), Some(icon::CHART_BAR));
        assert_eq!(icon_of("Server Status"), None);
        // Coloured entries go without.
        for label in ["Add User…", "Drop Index…", "Edit Document…", "Insert Document…", "Delete 2 Documents…"] {
            assert_eq!(icon_of(label), None, "{label}");
        }
        assert_eq!(icon_of("Kill Operation…"), None);
        assert_eq!(icon_of("Rebuild Indexes…"), None);
    }
}
