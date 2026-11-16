//! Markdown of the release notes, as GitHub shows them: headings, lists
//! (nested by their indent), paragraphs, `code`, **bold** and [links](url).

use egui::{RichText, Ui};

#[derive(Debug, PartialEq)]
pub enum Block<'a> {
    Heading(usize, &'a str),
    /// A list item and its depth.
    Item(usize, &'a str),
    Text(&'a str),
    Gap,
}

#[derive(Debug, PartialEq)]
pub enum Span<'a> {
    Text(&'a str),
    Code(&'a str),
    Bold(&'a str),
    Link(&'a str, &'a str),
}

pub fn blocks(text: &str) -> Vec<Block<'_>> {
    // Indents of the items above: an item deeper than the last one is in it.
    let mut indents: Vec<usize> = Vec::new();
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
            if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
                while indents.last().is_some_and(|&i| i >= indent) {
                    indents.pop();
                }
                indents.push(indent);
                return Block::Item(indents.len() - 1, item.trim_end());
            }
            indents.clear();
            if trimmed.is_empty() {
                Block::Gap
            } else if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
                Block::Heading(hashes, trimmed[hashes..].trim())
            } else {
                Block::Text(trimmed.trim_end())
            }
        })
        .collect()
}

pub fn spans(line: &str) -> Vec<Span<'_>> {
    let mut out = Vec::new();
    // Start of the text not yet out, and where to look for the next mark.
    let (mut text, mut at) = (0, 0);
    while let Some(i) = line[at..].find(['`', '*', '[']).map(|i| at + i) {
        let rest = &line[i..];
        let found = if let Some(r) = rest.strip_prefix('`') {
            r.find('`').map(|end| (Span::Code(&r[..end]), end + 2))
        } else if let Some(r) = rest.strip_prefix("**") {
            r.find("**").filter(|&end| end > 0).map(|end| (Span::Bold(&r[..end]), end + 4))
        } else if let Some(r) = rest.strip_prefix('[') {
            r.find("](").and_then(|mid| r[mid + 2..].find(')').map(|end| (Span::Link(&r[..mid], &r[mid + 2..mid + 2 + end]), mid + end + 4)))
        } else {
            None
        };
        match found {
            Some((span, len)) => {
                if text < i {
                    out.push(Span::Text(&line[text..i]));
                }
                out.push(span);
                at = i + len;
                text = at;
            }
            None => at = i + 1,
        }
    }
    if text < line.len() {
        out.push(Span::Text(&line[text..]));
    }
    out
}

pub fn show(ui: &mut Ui, text: &str) {
    let mut gap = true;
    for block in blocks(text) {
        match block {
            Block::Gap => {
                if !gap {
                    ui.add_space(4.0);
                }
                gap = true;
                continue;
            }
            Block::Heading(level, t) => {
                if !gap {
                    ui.add_space(6.0);
                }
                let size = ui.style().text_styles[&egui::TextStyle::Body].size + 2.0 * (4 - level.min(3)) as f32;
                ui.label(RichText::new(t).strong().size(size));
            }
            Block::Item(depth, t) => {
                ui.horizontal_top(|ui| {
                    ui.add_space(4.0 + 16.0 * depth as f32);
                    ui.label("•");
                    ui.vertical(|ui| line(ui, t));
                });
            }
            Block::Text(t) => line(ui, t),
        }
        gap = false;
    }
}

fn line(ui: &mut Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in spans(text) {
            match span {
                Span::Text(t) => ui.label(t),
                Span::Code(t) => ui.label(RichText::new(t).code()),
                Span::Bold(t) => ui.label(RichText::new(t).strong()),
                Span::Link(t, url) => ui.hyperlink_to(t, url),
            };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_of_release_notes() {
        let text = "## 1.5.0\n\n### Added\n\n- Ctrl+P: open\n- Tree views:\n  - Enter edits\n    - deeper\n  - Up/Down\n- Back\nSome text\n#nope";
        assert_eq!(
            blocks(text),
            [
                Block::Heading(2, "1.5.0"),
                Block::Gap,
                Block::Heading(3, "Added"),
                Block::Gap,
                Block::Item(0, "Ctrl+P: open"),
                Block::Item(0, "Tree views:"),
                Block::Item(1, "Enter edits"),
                Block::Item(2, "deeper"),
                Block::Item(1, "Up/Down"),
                Block::Item(0, "Back"),
                Block::Text("Some text"),
                Block::Text("#nope"),
            ]
        );
    }

    #[test]
    fn spans_of_a_line() {
        assert_eq!(
            spans("Double-click an `_id`, **R** or [docs](https://x.y/z) now"),
            [Span::Text("Double-click an "), Span::Code("_id"), Span::Text(", "), Span::Bold("R"), Span::Text(" or "), Span::Link("docs", "https://x.y/z"), Span::Text(" now")]
        );
        assert_eq!(spans("a * b, [not a link], `open"), [Span::Text("a * b, [not a link], `open")], "marks without their end");
        assert_eq!(spans("**"), [Span::Text("**")]);
        assert!(spans("").is_empty());
    }
}
