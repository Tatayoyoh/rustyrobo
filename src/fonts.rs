//! Font setup. egui's bundled fonts only cover Latin, Greek and Cyrillic: other
//! scripts (Hebrew, Arabic, CJK…) come from the fonts installed on the system,
//! as fallbacks after the bundled fonts, and the user can choose the first font
//! of each family.
//!
//! Scanning the system fonts is slow: it runs in a background task, the UI
//! starts with the bundled fonts. Font files are memory-mapped, so that a 20 MB
//! CJK font only costs the pages of the glyphs in use.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use egui::{FontData, FontDefinitions, FontFamily};
use fontdb::{Database, Family, ID, Query, Source, Stretch, Style, Weight};
use serde::{Deserialize, Serialize};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// One character of each script missing from the bundled fonts, with the fonts
/// to prefer for it (Linux, Windows, macOS). Scripts with none of them installed
/// use any font that has the character.
const SCRIPTS: &[(char, &[&str])] = &[
    ('\u{05D0}', &["Noto Sans Hebrew", "Segoe UI", "Arial Hebrew", "Arial", "DejaVu Sans"]),
    ('\u{0627}', &["Noto Sans Arabic", "Segoe UI", "Geeza Pro", "Arial", "DejaVu Sans"]),
    ('\u{4E00}', &["Noto Sans CJK SC", "Microsoft YaHei", "PingFang SC", "Hiragino Sans GB", "WenQuanYi Micro Hei"]),
    ('\u{3042}', &["Noto Sans CJK JP", "Yu Gothic", "Meiryo", "Hiragino Sans", "Hiragino Kaku Gothic ProN"]),
    ('\u{D55C}', &["Noto Sans CJK KR", "Malgun Gothic", "Apple SD Gothic Neo"]),
    ('\u{0E01}', &["Noto Sans Thai", "Leelawadee UI", "Thonburi"]),
    ('\u{0905}', &["Noto Sans Devanagari", "Nirmala UI", "Kohinoor Devanagari"]),
    ('\u{0985}', &["Noto Sans Bengali", "Nirmala UI", "Kohinoor Bangla"]),
    ('\u{0B85}', &["Noto Sans Tamil", "Nirmala UI", "Tamil Sangam MN"]),
    ('\u{0C85}', &["Noto Sans Kannada", "Nirmala UI", "Kannada Sangam MN"]),
    ('\u{0C05}', &["Noto Sans Telugu", "Nirmala UI", "Telugu Sangam MN"]),
    ('\u{0D05}', &["Noto Sans Malayalam", "Nirmala UI", "Malayalam Sangam MN"]),
    ('\u{0A85}', &["Noto Sans Gujarati", "Nirmala UI", "Gujarati Sangam MN"]),
    ('\u{0A05}', &["Noto Sans Gurmukhi", "Nirmala UI", "Gurmukhi Sangam MN"]),
    ('\u{0531}', &["Noto Sans Armenian", "Segoe UI", "DejaVu Sans"]),
    ('\u{10D0}', &["Noto Sans Georgian", "Segoe UI", "DejaVu Sans"]),
    ('\u{1200}', &["Noto Sans Ethiopic", "Ebrima", "Kefa"]),
    ('\u{1780}', &["Noto Sans Khmer", "Leelawadee UI", "Khmer Sangam MN"]),
    ('\u{0E81}', &["Noto Sans Lao", "Leelawadee UI", "Lao Sangam MN"]),
    ('\u{1000}', &["Noto Sans Myanmar", "Myanmar Text", "Myanmar Sangam MN"]),
    ('\u{0D85}', &["Noto Sans Sinhala", "Nirmala UI", "Sinhala Sangam MN"]),
    ('\u{0F40}', &["Noto Sans Tibetan", "Microsoft Himalaya", "Kailasa"]),
];

/// The fonts chosen by the user: `None` is the bundled font.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontChoice {
    pub ui: Option<String>,
    pub mono: Option<String>,
}

/// Families of the installed fonts, sorted, for the font pickers.
#[derive(Debug, Default)]
pub struct FontLists {
    pub all: Vec<String>,
    pub mono: Vec<String>,
}

struct System {
    db: Database,
    fallbacks: Vec<ID>,
    lists: FontLists,
}

static SYSTEM: OnceLock<System> = OnceLock::new();

/// The first call scans the system: not on the UI thread.
fn system() -> &'static System {
    SYSTEM.get_or_init(|| {
        let mut db = Database::new();
        db.load_system_fonts();
        let fallbacks = fallback_faces(&db);
        let lists = font_lists(&db);
        System { db, fallbacks, lists }
    })
}

/// `None` as long as the system fonts are not scanned.
pub fn lists() -> Option<&'static FontLists> {
    SYSTEM.get().map(|s| &s.lists)
}

/// Families of the system fonts, scanning them if needed: not on the UI thread.
#[cfg(test)]
pub fn lists_for_test() -> Vec<String> {
    system().lists.all.clone()
}

/// Bundled fonts: the Phosphor icons are a fallback of the text and monospace
/// fonts (regular only: one font, ~0.5 MB).
pub fn bundled() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        // Second, like in the proportional family: after the text font,
        // before egui's emoji fonts.
        mono.insert(mono.len().min(1), "phosphor".into());
    }
    fonts
}

/// Bundled fonts + the user's fonts + the fallbacks of the system.
pub fn definitions(choice: &FontChoice) -> FontDefinitions {
    let system = system();
    let mut fonts = bundled();
    let named = |fonts: &mut FontDefinitions, family: &Option<String>| {
        family.as_deref().and_then(|name| find_family(&system.db, name)).and_then(|id| add_face(&system.db, fonts, id))
    };
    let ui = named(&mut fonts, &choice.ui);
    let mono = named(&mut fonts, &choice.mono);
    let fallbacks: Vec<String> = system.fallbacks.iter().filter_map(|&id| add_face(&system.db, &mut fonts, id)).collect();
    chain_fonts(&mut fonts, ui, mono, &fallbacks);
    fonts
}

/// A chosen font comes first in its family, and is preferred to the system's
/// for the characters missing from the other family.
fn chain_fonts(fonts: &mut FontDefinitions, ui: Option<String>, mono: Option<String>, fallbacks: &[String]) {
    for (family, first, other) in [(FontFamily::Proportional, &ui, &mono), (FontFamily::Monospace, &mono, &ui)] {
        let Some(list) = fonts.families.get_mut(&family) else { continue };
        if let Some(first) = first {
            list.insert(0, first.clone());
        }
        list.extend(other.iter().filter(|o| Some(*o) != first.as_ref()).cloned());
        list.extend(fallbacks.iter().filter(|f| first.as_ref() != Some(*f) && other.as_ref() != Some(*f)).cloned());
    }
}

fn find_family(db: &Database, name: &str) -> Option<ID> {
    db.query(&Query { families: &[Family::Name(name)], weight: Weight::NORMAL, stretch: Stretch::Normal, style: Style::Normal })
}

/// Picks a face per script, reusing the faces picked for the previous scripts.
fn fallback_faces(db: &Database) -> Vec<ID> {
    let mut faces: Vec<ID> = Vec::new();
    for &(sample, preferred) in SCRIPTS {
        if faces.iter().any(|&id| has_char(db, id, sample)) {
            continue;
        }
        let id = preferred
            .iter()
            .filter_map(|name| find_family(db, name))
            .find(|&id| has_char(db, id, sample) && face_is_usable(db, id))
            .or_else(|| any_face_with(db, sample));
        if let Some(id) = id {
            faces.push(id);
        }
    }
    faces
}

/// A regular face with the character, preferably sans-serif and proportional.
fn any_face_with(db: &Database, c: char) -> Option<ID> {
    db.faces()
        .filter(|f| f.weight == Weight::NORMAL && f.style == Style::Normal && f.stretch == Stretch::Normal)
        .filter(|f| has_char(db, f.id, c) && face_is_usable(db, f.id))
        .min_by_key(|f| (f.monospaced, !f.families.iter().any(|(name, _)| name.contains("Sans"))))
        .map(|f| f.id)
}

fn has_char(db: &Database, id: ID, c: char) -> bool {
    db.with_face_data(id, |data, index| FontRef::from_index(data, index).is_ok_and(|font| font.charmap().map(c).is_some())).unwrap_or(false)
}

/// egui panics on a font it cannot read, and draws nothing with a font without
/// outlines (colour bitmaps): such a file must never reach it.
fn is_usable(data: &[u8], index: u32) -> bool {
    FontRef::from_index(data, index).is_ok_and(|font| font.outline_glyphs().get(GlyphId::new(1)).is_some())
}

fn face_is_usable(db: &Database, id: ID) -> bool {
    db.with_face_data(id, is_usable).unwrap_or(false)
}

fn font_lists(db: &Database) -> FontLists {
    let mut lists = FontLists::default();
    for face in db.faces().filter(|f| face_is_usable(db, f.id)) {
        let Some((name, _)) = face.families.first() else { continue };
        lists.all.push(name.clone());
        if face.monospaced {
            lists.mono.push(name.clone());
        }
    }
    for list in [&mut lists.all, &mut lists.mono] {
        list.sort_by_key(|name| name.to_lowercase());
        list.dedup();
    }
    lists
}

/// Adds a face to the definitions and returns its font name.
fn add_face(db: &Database, fonts: &mut FontDefinitions, id: ID) -> Option<String> {
    let face = db.face(id)?;
    let data: &'static [u8] = match &face.source {
        Source::File(path) => map_file(path)?,
        Source::Binary(_) | Source::SharedFile(..) => {
            let bytes = db.with_face_data(id, |data, _| data.to_vec())?;
            Box::leak(bytes.into_boxed_slice())
        }
    };
    if !is_usable(data, face.index) {
        return None;
    }
    let name = format!("{}#{}", face.post_script_name, face.index);
    let mut font = FontData::from_static(data);
    font.index = face.index;
    fonts.font_data.insert(name.clone(), Arc::new(font));
    Some(name)
}

/// Mappings live until the end of the process: egui wants `'static` data and the
/// same few fonts are mapped again each time the font choice changes.
fn map_file(path: &Path) -> Option<&'static [u8]> {
    static MAPS: Mutex<Option<HashMap<PathBuf, &'static [u8]>>> = Mutex::new(None);
    let mut maps = MAPS.lock().unwrap_or_else(|e| e.into_inner());
    let maps = maps.get_or_insert_default();
    if let Some(data) = maps.get(path) {
        return Some(data);
    }
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: font files of the system are not modified while the app runs. A
    // truncated file could fault, like in every program that maps its fonts.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let data: &'static [u8] = Box::leak(Box::new(map));
    maps.insert(path.to_path_buf(), data);
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Database holding only a bundled font: no dependency on the system fonts.
    fn hack_only() -> Database {
        let fonts = FontDefinitions::default();
        let mut db = Database::new();
        db.load_font_data(fonts.font_data["Hack"].font.to_vec());
        db
    }

    fn family(fonts: &FontDefinitions, family: FontFamily) -> Vec<String> {
        fonts.families[&family].clone()
    }

    #[test]
    fn bundled_fonts_have_icons_after_the_text_font() {
        let fonts = bundled();
        assert_eq!(family(&fonts, FontFamily::Proportional)[1], "phosphor");
        assert_eq!(family(&fonts, FontFamily::Monospace)[1], "phosphor");
    }

    #[test]
    fn chosen_fonts_come_first_and_fallbacks_last() {
        let mut fonts = bundled();
        let ui = Some("ui-font".to_string());
        let mono = Some("mono-font".to_string());
        chain_fonts(&mut fonts, ui, mono, &["system-font".to_string()]);
        let prop = family(&fonts, FontFamily::Proportional);
        assert_eq!(prop[0], "ui-font");
        assert_eq!(&prop[prop.len() - 2..], ["mono-font", "system-font"]);
        let mono = family(&fonts, FontFamily::Monospace);
        assert_eq!(mono[0], "mono-font");
        assert_eq!(&mono[mono.len() - 2..], ["ui-font", "system-font"]);
    }

    #[test]
    fn chain_without_duplicates() {
        // The same font everywhere: it must appear once per family, or egui
        // would look it up twice for each missing character.
        let mut fonts = bundled();
        let same = Some("same".to_string());
        chain_fonts(&mut fonts, same.clone(), same, &["same".to_string()]);
        for f in [FontFamily::Proportional, FontFamily::Monospace] {
            assert_eq!(family(&fonts, f).iter().filter(|n| *n == "same").count(), 1);
        }
    }

    #[test]
    fn a_font_is_picked_once_and_only_for_the_scripts_it_has() {
        // Hack has Armenian and Georgian, and none of the other scripts.
        let db = hack_only();
        let id = db.faces().next().unwrap().id;
        let covered: Vec<char> = SCRIPTS.iter().map(|s| s.0).filter(|&c| has_char(&db, id, c)).collect();
        assert_eq!(covered, ['\u{0531}', '\u{10D0}']);
        assert_eq!(fallback_faces(&db), [id]);
        assert!(fallback_faces(&Database::new()).is_empty());
    }

    #[test]
    fn coverage_comes_from_the_character_map() {
        let db = hack_only();
        let id = db.faces().next().unwrap().id;
        assert!(has_char(&db, id, 'a'));
        assert!(!has_char(&db, id, '\u{05D0}'));
        assert_eq!(any_face_with(&db, 'a'), Some(id));
        assert_eq!(any_face_with(&db, '\u{05D0}'), None);
    }

    #[test]
    fn unreadable_fonts_are_refused() {
        let fonts = FontDefinitions::default();
        let hack = &fonts.font_data["Hack"].font;
        assert!(is_usable(hack, 0));
        assert!(!is_usable(hack, 1), "no such face in the file");
        assert!(!is_usable(&hack[..64], 0), "truncated");
        assert!(!is_usable(b"not a font", 0));
        assert!(!is_usable(&[], 0));
    }

    #[test]
    fn lists_are_sorted_and_flag_monospaced_fonts() {
        let lists = font_lists(&hack_only());
        assert_eq!(lists.all, ["Hack"]);
        assert_eq!(lists.mono, ["Hack"]);
    }

    #[test]
    fn faces_from_memory_can_be_added() {
        let db = hack_only();
        let id = db.faces().next().unwrap().id;
        let mut fonts = FontDefinitions::default();
        let name = add_face(&db, &mut fonts, id).unwrap();
        assert!(fonts.font_data.contains_key(&name));
    }

    /// Context with the fonts in use from now on.
    fn context_with(fonts: FontDefinitions) -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        ctx
    }

    #[test]
    fn scripts_have_distinct_samples_missing_from_the_bundled_fonts() {
        let ctx = context_with(bundled());
        for (i, &(c, _)) in SCRIPTS.iter().enumerate() {
            assert!(!SCRIPTS[..i].iter().any(|&(o, _)| o == c), "{c} listed twice");
            let id = egui::FontId::proportional(13.0);
            assert!(!ctx.fonts_mut(|f| f.has_glyph(&id, c)), "{c} is already bundled");
        }
    }

    #[test]
    fn system_definitions_cover_hebrew_when_a_font_has_it() {
        // Depends on the machine: only checks what the database claims.
        let system = system();
        let has_font = any_face_with(&system.db, '\u{05D0}').is_some();
        let ctx = context_with(definitions(&FontChoice::default()));
        let id = egui::FontId::monospace(13.0);
        assert_eq!(ctx.fonts_mut(|f| f.has_glyph(&id, '\u{05D0}')), has_font);
    }
}
