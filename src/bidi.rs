//! Right-to-left text in read-only labels. egui lays text out left to right:
//! it shapes each run of a font in the direction of its script (so that a Hebrew
//! word comes out right) but keeps the runs in logical order (so that the words
//! of a sentence come out reversed), see `TODO: heed bidi` in epaint.
//!
//! [`layout_job`] applies the Unicode bidirectional algorithm to give egui the
//! pieces of the text in visual order, one section per piece: each is shaped on
//! its own, whatever the fonts in use. The text of the job is reordered, which
//! rules out editors: what they type would not go where the cursor shows it.
//! Read-only views can use it ([`reorder_sections`]), they copy from their own
//! text, not from the job.

use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat};
use egui::{Color32, FontId};
use unicode_bidi::{BidiClass, BidiInfo, Direction, bidi_class, get_base_direction};

/// Whether the text needs [`layout_job`]. Most text is ASCII: this is cheap
/// enough to be called for each visible cell at each frame.
pub fn has_rtl(text: &str) -> bool {
    !text.is_ascii() && text.chars().any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL | BidiClass::RLE | BidiClass::RLO | BidiClass::RLI))
}

/// Whether the text reads from right to left: its start is then on the right.
pub fn starts_rtl(text: &str) -> bool {
    get_base_direction(text) == Direction::Rtl
}

/// The pieces of the text from left to right. An RTL word stays in logical
/// order, since egui reverses it when it shapes it.
pub fn visual_pieces(text: &str) -> Vec<String> {
    let info = BidiInfo::new(text, None);
    let mut out = Vec::new();
    for para in &info.paragraphs {
        // The line break stays at the end of the line, whatever its direction.
        let line = text[para.range.clone()].trim_end_matches(|c| bidi_class(c) == BidiClass::B);
        let line = para.range.start..para.range.start + line.len();
        if !line.is_empty() {
            let (levels, runs) = info.visual_runs(para, line.clone());
            for run in runs {
                let piece = &text[run.clone()];
                if levels[run.start].is_rtl() {
                    out.extend(rtl_tokens(piece).into_iter().rev().map(mirrored));
                } else if piece.contains(is_arabic_script_digit) {
                    out.extend(piece.chars().map(String::from));
                } else {
                    out.push(piece.to_string());
                }
            }
        }
        if line.end < para.range.end {
            out.push(text[line.end..para.range.end].to_string());
        }
    }
    out
}

/// These digits read from left to right but are of the Arabic script: egui would
/// shape a whole number from right to left, one digit at a time it cannot.
fn is_arabic_script_digit(c: char) -> bool {
    ('\u{06F0}'..='\u{06F9}').contains(&c)
}

/// Words (with their combining marks) and single characters in between.
fn rtl_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut word_start = None;
    for (i, c) in text.char_indices() {
        let in_word = matches!(bidi_class(c), BidiClass::R | BidiClass::AL) || (word_start.is_some() && bidi_class(c) == BidiClass::NSM);
        if in_word {
            word_start.get_or_insert(i);
            continue;
        }
        if let Some(start) = word_start.take() {
            tokens.push(&text[start..i]);
        }
        tokens.push(&text[i..i + c.len_utf8()]);
    }
    if let Some(start) = word_start {
        tokens.push(&text[start..]);
    }
    tokens
}

/// An opening bracket of an RTL run opens to the right: it is drawn as a closing
/// one. egui would not do it, a bracket being shaped left to right on its own.
fn mirrored(token: &str) -> String {
    token
        .chars()
        .map(|c| match c {
            '(' => ')',
            ')' => '(',
            '[' => ']',
            ']' => '[',
            '{' => '}',
            '}' => '{',
            '<' => '>',
            '>' => '<',
            '«' => '»',
            '»' => '«',
            c => c,
        })
        .collect()
}

/// Not `job.append`: it merges the sections of the same format.
fn push_section(job: &mut LayoutJob, text: &str, format: TextFormat) {
    let start = job.text.len();
    job.text += text;
    job.sections.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(start)..ByteIndex(job.text.len()), format });
}

pub fn layout_job(text: &str, font: FontId, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for piece in visual_pieces(text) {
        push_section(&mut job, &piece, TextFormat::simple(font.clone(), color));
    }
    job
}

/// Puts the RTL text of each section in visual order. Sections stay in place: in
/// highlighted code, a string reads from right to left between its neighbours.
/// The job keeps the same characters, so that lines and offsets still match.
pub fn reorder_sections(mut job: LayoutJob) -> LayoutJob {
    if !has_rtl(&job.text) {
        return job;
    }
    let text = std::mem::take(&mut job.text);
    for section in std::mem::take(&mut job.sections) {
        let part = &text[section.byte_range.start.0..section.byte_range.end.0];
        if has_rtl(part) {
            for piece in visual_pieces(part) {
                push_section(&mut job, &piece, section.format.clone());
            }
        } else {
            push_section(&mut job, part, section.format);
        }
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = "שלום";
    const WORLD: &str = "עולם";

    #[test]
    fn detects_rtl_text() {
        assert!(has_rtl(HELLO));
        assert!(has_rtl("abc سلام"));
        assert!(!has_rtl("abc 123"));
        assert!(!has_rtl("Привет 你好 é"));
    }

    #[test]
    fn base_direction_comes_from_the_first_strong_character() {
        assert!(starts_rtl(&format!("123 {HELLO} abc")));
        assert!(!starts_rtl(&format!("abc {HELLO}")));
    }

    #[test]
    fn words_of_a_sentence_go_from_right_to_left() {
        assert_eq!(visual_pieces(&format!("{HELLO} {WORLD}")), [WORLD, " ", HELLO]);
    }

    #[test]
    fn numbers_stay_left_to_right() {
        assert_eq!(visual_pieces(&format!("{HELLO} 123 {WORLD}")), [WORLD, " ", "123", " ", HELLO]);
    }

    #[test]
    fn persian_digits_are_split_so_that_egui_cannot_reverse_them() {
        assert_eq!(visual_pieces(&format!("{HELLO} ۱۲")), ["۱", "۲", " ", HELLO]);
        assert_eq!(visual_pieces(&format!("{HELLO} ١٢")), ["١٢", " ", HELLO]);
    }

    #[test]
    fn latin_inside_rtl_keeps_its_order_and_the_first_strong_character_sets_the_direction() {
        assert_eq!(visual_pieces(&format!("{HELLO} abc def")), ["abc def", " ", HELLO]);
        assert_eq!(visual_pieces(&format!("abc def {HELLO}")), ["abc def ", HELLO]);
    }

    #[test]
    fn brackets_are_mirrored_in_rtl_runs_only() {
        // Logically "HELLO (WORLD)": the opening bracket is drawn on the right
        // of WORLD, as a shape that opens towards it.
        assert_eq!(visual_pieces(&format!("{HELLO} ({WORLD})")), ["(", WORLD, ")", " ", HELLO]);
        assert_eq!(visual_pieces(&format!("(abc) {HELLO}")), ["(abc) ", HELLO]);
    }

    #[test]
    fn combining_marks_stay_on_their_letter() {
        // Niqqud: the word must not be cut, the shaper positions the marks.
        let pointed = "שָׁלוֹם";
        assert_eq!(visual_pieces(&format!("{pointed} {WORLD}")), [WORLD, " ", pointed]);
    }

    #[test]
    fn arabic_words_stay_whole_for_letter_joining() {
        assert_eq!(visual_pieces("مرحبا بالعالم"), ["بالعالم", " ", "مرحبا"]);
    }

    #[test]
    fn lines_keep_their_order() {
        let (a, b) = ("א ב", "ג ד");
        assert_eq!(visual_pieces(&format!("{a}\n{b}\n")), ["ב", " ", "א", "\n", "ד", " ", "ג", "\n"]);
        assert_eq!(visual_pieces(&format!("{a}\r\n\n{b}")), ["ב", " ", "א", "\r", "\n", "\n", "ד", " ", "ג"]);
    }

    #[test]
    fn text_is_preserved() {
        for text in ["", " ", HELLO, "a\tb", "א\rב", "\u{202E}abc\u{202C}", "(א) [ב] {ג}", "א\u{05BC}", "\u{05BC}", "1 א 2 ב 3"] {
            let chars = |s: &str| {
                let mut v: Vec<char> = s.chars().map(|c| if "()[]{}<>".contains(c) { '#' } else { c }).collect();
                v.sort_unstable();
                v
            };
            assert_eq!(chars(&visual_pieces(text).concat()), chars(text), "{text:?}");
        }
    }

    /// Lays out with the fonts of the system, if one has Hebrew letters. Returns
    /// the characters from right to left, as drawn by egui.
    fn read_from_the_right(fonts: egui::FontDefinitions, job: LayoutJob) -> Option<String> {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        if !ctx.fonts_mut(|f| f.has_glyph(&FontId::monospace(13.0), '\u{05D0}')) {
            return None;
        }
        let galley = ctx.fonts_mut(|f| f.layout_job(job));
        let mut glyphs: Vec<_> = galley.rows.iter().flat_map(|r| r.glyphs.iter()).filter(|g| g.advance_width > 0.0).map(|g| (g.pos.x, g.chr)).collect();
        glyphs.sort_by(|a, b| b.0.total_cmp(&a.0));
        Some(glyphs.into_iter().map(|g| g.1).collect())
    }

    #[test]
    fn egui_draws_the_job_in_reading_order() {
        let fonts = crate::fonts::definitions(&Default::default());
        let text = format!("{HELLO} {WORLD} {HELLO}");
        let job = layout_job(&text, FontId::monospace(13.0), Color32::RED);
        if let Some(drawn) = read_from_the_right(fonts, job) {
            assert_eq!(drawn, text);
        }
    }

    #[test]
    fn egui_draws_the_job_in_reading_order_with_a_font_having_latin_and_hebrew() {
        // A single font for the letters and the spaces: egui would shape the
        // whole text as one run if the sections were merged.
        let ui = crate::fonts::lists_for_test().into_iter().find(|f| ["DejaVu Sans", "Arial", "Segoe UI"].contains(&f.as_str()));
        let Some(ui) = ui else { return };
        let fonts = crate::fonts::definitions(&crate::fonts::FontChoice { ui: Some(ui.clone()), mono: Some(ui) });
        let text = format!("{HELLO} {WORLD} 123 abc");
        let job = layout_job(&text, FontId::monospace(13.0), Color32::RED);
        if let Some(drawn) = read_from_the_right(fonts, job) {
            // Left to right inside the numbers and the Latin text.
            assert_eq!(drawn, format!("{HELLO} {WORLD} 321 cba"));
        }
    }

    #[test]
    fn reordering_sections_keeps_their_format_and_the_other_sections() {
        let (key, string) = (Color32::RED, Color32::GREEN);
        let font = FontId::monospace(13.0);
        let mut job = LayoutJob::default();
        job.append("\"key\" : ", 0.0, TextFormat::simple(font.clone(), key));
        job.append(&format!("\"{HELLO} {WORLD}\""), 0.0, TextFormat::simple(font.clone(), string));
        job.append(",\n", 0.0, TextFormat::simple(font, key));
        let chars = job.text.chars().count();
        let job = reorder_sections(job);
        assert_eq!(job.text, format!("\"key\" : \"{WORLD} {HELLO}\",\n"));
        assert_eq!(job.text.chars().count(), chars);
        let colors: Vec<(&str, Color32)> = job.sections.iter().map(|s| (&job.text[s.byte_range.start.0..s.byte_range.end.0], s.format.color)).collect();
        assert_eq!(colors, [("\"key\" : ", key), ("\"", string), (WORLD, string), (" ", string), (HELLO, string), ("\"", string), (",\n", key)]);
    }

    #[test]
    fn reordering_leaves_other_jobs_alone() {
        let mut job = LayoutJob::default();
        job.append("Latin и кириллица", 0.0, TextFormat::simple(FontId::monospace(13.0), Color32::RED));
        let sections = job.sections.len();
        let job = reorder_sections(job);
        assert_eq!(job.text, "Latin и кириллица");
        assert_eq!(job.sections.len(), sections);
    }

    #[test]
    fn job_has_one_section_per_piece() {
        let job = layout_job(&format!("{HELLO} {WORLD}"), FontId::monospace(13.0), Color32::RED);
        assert_eq!(job.sections.len(), 3);
        assert_eq!(job.text, format!("{WORLD} {HELLO}"));
    }
}
