//! Editing helpers behind the code editors: block folding (including edits
//! made on the folded text), text search and multiple selections (Ctrl+D).
//! Positions are char indices unless stated otherwise.

use std::collections::HashSet;
use std::ops::Range;

// ------------------------------------------------------------------ folding

/// Text with some blocks folded into one `{…}` line.
#[derive(Clone, Default)]
pub struct FoldedText {
    pub text: String,
    /// Source line of each displayed line.
    pub lines: Vec<usize>,
    /// `Some(folded)` for displayed lines that open a block.
    pub marks: Vec<Option<bool>>,
    /// Source lines opening a block, with their nesting depth.
    pub openers: Vec<(usize, usize)>,
    /// Folded parts: byte range in `text`, byte range in the source, first line.
    hidden: Vec<(Range<usize>, Range<usize>, usize)>,
}

impl FoldedText {
    /// Lines folded by "Collapse All": every document when there are
    /// several, else the first-level blocks of the only one.
    pub fn collapse_all(&self) -> HashSet<usize> {
        let docs = self.openers.iter().filter(|o| o.1 == 0).count();
        let depth = usize::from(docs <= 1);
        self.openers.iter().filter(|o| o.1 == depth).map(|o| o.0).collect()
    }

    /// Source byte offset of a byte offset of `text`, `Err(line)` when it
    /// falls inside the fold starting at `line`.
    fn to_source(&self, pos: usize) -> Result<usize, usize> {
        let mut delta = 0isize;
        for (d, s, line) in &self.hidden {
            if pos <= d.start {
                break;
            }
            if pos < d.end {
                return Err(*line);
            }
            delta = s.end as isize - d.end as isize;
        }
        Ok((pos as isize + delta) as usize)
    }
}

/// Folds `text` at the blocks starting on the `folded` lines. Blocks are
/// found from the brackets ending/starting lines, which is exact for the
/// output of `to_shell_json` (one key or element per line).
pub fn fold_text(text: &str, folded: &HashSet<usize>) -> FoldedText {
    let src: Vec<&str> = text.split('\n').collect();
    let mut starts = Vec::with_capacity(src.len());
    let mut offset = 0;
    for l in &src {
        starts.push(offset);
        offset += l.len() + 1;
    }
    let mut ends = vec![None; src.len()];
    let mut out = FoldedText { text: String::with_capacity(text.len()), ..Default::default() };
    if !text.contains('\r') {
        let mut stack = Vec::new();
        for (i, l) in src.iter().enumerate() {
            let t = l.trim();
            if (t.starts_with('}') || t.starts_with(']'))
                && let Some(open) = stack.pop()
            {
                ends[open] = Some(i);
            }
            if t.ends_with('{') || t.ends_with('[') {
                out.openers.push((i, stack.len()));
                stack.push(i);
            }
        }
    }
    let mut i = 0;
    while i < src.len() {
        out.lines.push(i);
        out.text.push_str(src[i]);
        match ends[i] {
            Some(end) if folded.contains(&i) => {
                let d = out.text.len();
                out.text.push('…');
                out.text.push_str(src[end].trim_start());
                out.hidden.push((d..out.text.len(), starts[i] + src[i].len()..starts[end] + src[end].len(), i));
                out.marks.push(Some(true));
                i = end + 1;
            }
            end => {
                out.marks.push(end.map(|_| false));
                i += 1;
            }
        }
        if i < src.len() {
            out.text.push('\n');
        }
    }
    out
}

/// Applies to `source` the edit that turned `view.text` into `edited`, and
/// moves the `folded` lines below it. An edit inside a folded part is
/// dropped and unfolds that block instead. Returns whether `source` changed.
pub fn apply_folded_edit(source: &mut String, view: &FoldedText, edited: &str, folded: &mut HashSet<usize>) -> bool {
    let old = view.text.as_str();
    let prefix: usize = old.chars().zip(edited.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum();
    let max_suffix = old.len().min(edited.len()) - prefix;
    let suffix: usize = old[prefix..]
        .chars()
        .rev()
        .zip(edited[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .scan(0, |n, l| {
            *n += l;
            (*n <= max_suffix).then_some(l)
        })
        .sum();
    let inserted = &edited[prefix..edited.len() - suffix];
    let (start, end) = match (view.to_source(prefix), view.to_source(old.len() - suffix)) {
        (Ok(s), Ok(e)) => (s, e),
        (Err(line), _) | (_, Err(line)) => {
            folded.remove(&line);
            return false;
        }
    };
    let line = source[..start].matches('\n').count();
    let removed = source[start..end].matches('\n').count();
    let added = inserted.matches('\n').count();
    // An edit on a folded line, before its `{`, moves the block down.
    let at_line_end = source[start..].starts_with('\n') || start == source.len();
    source.replace_range(start..end, inserted);
    *folded = folded
        .iter()
        .filter_map(|&f| match f {
            f if f < line || (f == line && at_line_end) => Some(f),
            f if f == line => (removed == 0).then_some(f + added),
            f if f <= line + removed => None,
            f => Some(f - removed + added),
        })
        .collect();
    true
}

// ------------------------------------------------------------------- search

pub const MAX_MATCHES: usize = 10_000;

/// Case-insensitive (ASCII) occurrences of `query`: char ranges, with the
/// line of each.
pub fn find_all(text: &str, query: &str) -> Vec<(Range<usize>, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let hay = text.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let (mut byte, mut ch, mut line) = (0, 0, 0);
    let mut out = Vec::new();
    for (b, m) in hay.match_indices(&needle).take(MAX_MATCHES) {
        let skipped = &text[byte..b];
        ch += skipped.chars().count();
        line += skipped.matches('\n').count();
        byte = b;
        out.push((ch..ch + text[b..b + m.len()].chars().count(), line));
    }
    out
}

// --------------------------------------------------------- multi-selections

/// A selection: (anchor, head).
pub type Sel = (usize, usize);

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The word around the cursor `at`.
pub fn word_at(text: &str, at: usize) -> Option<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let mut start = at.min(chars.len());
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at.min(chars.len());
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    (start < end).then_some(start..end)
}

/// Next occurrence of the text of the last selection, after it (wrapping
/// around), that is not selected yet.
pub fn next_occurrence(text: &str, sels: &[Sel]) -> Option<Range<usize>> {
    let &(a, h) = sels.last()?;
    let (lo, hi) = (a.min(h), a.max(h));
    let chars: Vec<char> = text.chars().collect();
    if lo == hi || hi > chars.len() {
        return None;
    }
    let needle = &chars[lo..hi];
    let n = chars.len();
    let taken = |s: usize| sels.iter().any(|&(a, h)| a.min(h) == s);
    (1..=n)
        .map(|k| (hi - needle.len() + k) % n)
        .find(|&s| s + needle.len() <= n && &chars[s..s + needle.len()] == needle && !taken(s))
        .map(|s| s..s + needle.len())
}

pub enum Edit<'a> {
    Insert(&'a str),
    Backspace,
    Delete,
    Left,
    Right,
}

/// Applies `edit` at every selection; selections become cursors.
pub fn apply_edit(text: &mut String, sels: &mut Vec<Sel>, edit: Edit<'_>) {
    let mut sorted: Vec<(usize, usize)> = sels.iter().map(|&(a, h)| (a.min(h), a.max(h))).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut delta = 0isize;
    let mut out = Vec::with_capacity(sorted.len());
    for (lo, hi) in sorted {
        let (mut lo, mut hi) = ((lo as isize + delta) as usize, (hi as isize + delta) as usize);
        let len = text.chars().count();
        let cursor = match edit {
            Edit::Left => if lo == hi { lo.saturating_sub(1) } else { lo },
            Edit::Right => if lo == hi { (hi + 1).min(len) } else { hi },
            Edit::Insert(_) | Edit::Backspace | Edit::Delete => {
                if lo == hi {
                    match edit {
                        Edit::Backspace => lo = lo.saturating_sub(1),
                        Edit::Delete => hi = (hi + 1).min(len),
                        _ => {}
                    }
                }
                let ins = if let Edit::Insert(s) = edit { s } else { "" };
                let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(b, _)| b);
                let (bl, bh) = (byte(lo), byte(hi));
                text.replace_range(bl..bh, ins);
                let n = ins.chars().count();
                delta += n as isize - (hi - lo) as isize;
                lo + n
            }
        };
        out.push((cursor, cursor));
    }
    out.dedup();
    *sels = out;
}

/// Selected texts, in document order, one per line (for copy).
pub fn selected_text(text: &str, sels: &[Sel]) -> String {
    let mut ranges: Vec<(usize, usize)> = sels.iter().map(|&(a, h)| (a.min(h), a.max(h))).collect();
    ranges.sort_unstable();
    ranges.iter().map(|&(lo, hi)| text.chars().skip(lo).take(hi - lo).collect::<String>()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "{\n    \"a\" : {\n        \"b\" : [\n            1\n        ]\n    },\n    \"s\" : \"x\"\n}";

    #[test]
    fn folds_and_unfolds() {
        let open = fold_text(DOC, &HashSet::new());
        assert_eq!(open.text, DOC);
        assert_eq!(open.openers, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(open.collapse_all(), HashSet::from([1]));
        let f = fold_text(DOC, &HashSet::from([1]));
        assert_eq!(f.text, "{\n    \"a\" : {…},\n    \"s\" : \"x\"\n}");
        assert_eq!(f.lines, vec![0, 1, 6, 7]);
        assert_eq!(f.marks, vec![Some(false), Some(true), None, None]);
        assert_eq!(fold_text(DOC, &HashSet::from([0])).text, "{…}");
        let two = format!("{DOC}\n\n{DOC}");
        assert_eq!(fold_text(&two, &HashSet::new()).collapse_all(), HashSet::from([0, 9]));
    }

    #[test]
    fn edits_through_folds() {
        let folded0 = HashSet::from([1]);
        let edit = |from: &str, to: &str| {
            let mut src = DOC.to_string();
            let mut folded = folded0.clone();
            let view = fold_text(&src, &folded);
            let edited = view.text.replacen(from, to, 1);
            let ok = apply_folded_edit(&mut src, &view, &edited, &mut folded);
            (ok, src, folded)
        };
        // Typing after the fold keeps the hidden part.
        let (ok, src, folded) = edit("\"s\" : \"x\"", "\"s\" : \"xy\"");
        assert!(ok);
        assert_eq!(src, DOC.replace("\"x\"", "\"xy\""));
        assert_eq!(folded, folded0);
        // A new line above shifts the fold.
        let (ok, src, folded) = edit("{\n", "{\n    \"n\" : 1,\n");
        assert!(ok && src.contains("\"n\" : 1,\n    \"a\" : {\n        \"b\""));
        assert_eq!(folded, HashSet::from([2]));
        // Right after the placeholder.
        let (ok, src, _) = edit("{…},", "{…},\n    \"t\" : 2,");
        assert!(ok && src.contains("    },\n    \"t\" : 2,\n    \"s\""), "{src}");
        // Deleting the folded line removes the hidden block too.
        let (ok, src, folded) = edit("    \"a\" : {…},\n", "");
        assert!(ok);
        assert_eq!(src, "{\n    \"s\" : \"x\"\n}");
        assert_eq!(fold_text(&src, &folded).text, src, "no block left to fold");
        // Typing inside the placeholder unfolds instead.
        let (ok, src, folded) = edit("{…}", "{…x}");
        assert!(!ok && src == DOC && folded.is_empty());
    }

    #[test]
    fn finds_matches() {
        let m = find_all("Abc\nxaBc é abc", "abc");
        assert_eq!(m, vec![(0..3, 0), (5..8, 1), (11..14, 1)]);
        assert!(find_all("abc", "").is_empty());
    }

    #[test]
    fn multi_selection() {
        let text = "foo.bar(foo, foo)";
        assert_eq!(word_at(text, 1), Some(0..3));
        assert_eq!(word_at(text, 3), Some(0..3));
        assert_eq!(word_at("a  b", 2), None);
        let mut sels = vec![(0, 3)];
        let n = next_occurrence(text, &sels).unwrap();
        assert_eq!(n, 8..11);
        sels.push((n.start, n.end));
        let n = next_occurrence(text, &sels).unwrap();
        sels.push((n.start, n.end));
        assert_eq!(next_occurrence(text, &sels), None, "all selected");
        assert_eq!(selected_text(text, &sels), "foo\nfoo\nfoo");

        let mut t = text.to_string();
        apply_edit(&mut t, &mut sels, Edit::Insert("qux"));
        assert_eq!(t, "qux.bar(qux, qux)");
        assert_eq!(sels, vec![(3, 3), (11, 11), (16, 16)]);
        apply_edit(&mut t, &mut sels, Edit::Backspace);
        assert_eq!(t, "qu.bar(qu, qu)");
        apply_edit(&mut t, &mut sels, Edit::Left);
        apply_edit(&mut t, &mut sels, Edit::Delete);
        assert_eq!(t, "q.bar(q, q)");
        apply_edit(&mut t, &mut sels, Edit::Right);
        assert_eq!(sels, vec![(2, 2), (8, 8), (11, 11)]);
    }
}
