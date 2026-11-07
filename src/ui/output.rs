//! Result panes: tree / table / text views of documents, paging and the
//! document context menu (view, edit, insert, delete, copy).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use bson::{Bson, doc};
use egui::{Align, FontId, Id, Layout, RichText, Ui};
use egui_extras::{Column, TableBuilder};

use crate::app::{Action, Refresh};
use crate::bsonfmt::{self, FormatOptions};
use crate::settings::ViewMode;
use crate::shell::{OutputKind, ShellOutput};
use crate::theme::{Theme, bson_color};
use crate::ui::dialogs::{Dialog, DocMode};
use crate::ui::editing::{FoldedText, MAX_MATCHES, find_all, fold_text};
use crate::ui::editor::{FoldClick, TextMatches};
use crate::ui::{icon, menu};

pub struct OutputPane {
    pub output: ShellOutput,
    pub mode: ViewMode,
    pub loading: bool,
    pub skip: u64,
    pub page_size: u64,
    pub expanded: HashSet<String>,
    pub selected: Selection,
    /// Derived views, rebuilt only when the page, the expansion state, the
    /// format options or the theme change (not every frame).
    text_cache: Option<(FormatOptions, String)>,
    /// Folded blocks of the text view (first line of each, in `text_cache`).
    folded: HashSet<usize>,
    fold_cache: Option<FoldedText>,
    rows_cache: Option<(FormatOptions, &'static str, Vec<Row>)>,
    columns_cache: Option<(Vec<String>, bool)>,
    skip_input: String,
    size_input: String,
    /// The pointer was over this pane in the last frame.
    pub pointer_over: bool,
    /// The value under the pointer in the last frame.
    pub hovered: Option<HoveredValue>,
    find: PaneFind,
    /// Value being edited in place, in the table or the tree.
    pub cell_edit: Option<CellEdit>,
    /// Table cell of the last right-click, for its context menu.
    menu_cell: Option<(usize, String)>,
}

/// Why an object or an array cannot be edited in place.
const CONTAINER: &str = "Objects and arrays are edited with Edit Document…";

/// A value edited in place: a field of a document (a cell of the table), or
/// any value inside it (a row of the tree).
#[derive(Debug, Clone, PartialEq)]
pub struct CellEdit {
    pub doc: usize,
    /// Keys leading to the value, as in the tree (`[2]` for an array element).
    pub keys: Vec<String>,
    /// The value in shell syntax.
    pub text: String,
    pub error: Option<String>,
    focused: bool,
}

/// A value of the tree or the table: `keys` lead to it inside the document.
pub struct HoveredValue {
    doc: usize,
    keys: Vec<String>,
    path: String,
}

/// Ctrl+F in a result pane.
#[derive(Default)]
struct PaneFind {
    open: bool,
    query: String,
    focus: bool,
    /// Escape closes the bar, but egui drops the focus before the widgets run.
    had_focus: bool,
    current: usize,
    /// Frames left to bring the current match into view: a row has to be
    /// scrolled to before its cell can be.
    scroll: u8,
    /// What `hits` were computed for.
    key: Option<(String, ViewMode, FormatOptions)>,
    /// The page or the shown text has changed.
    stale: bool,
    hits: Hits,
}

/// Matches of a search, in the terms of the current view.
#[derive(Default)]
enum Hits {
    #[default]
    None,
    Rows { list: Vec<RowHit>, by_path: HashMap<String, usize> },
    Cells { list: Vec<(usize, String)>, by_cell: HashMap<(usize, String), usize> },
    Text(Vec<(Range<usize>, usize)>),
}

impl Hits {
    fn len(&self) -> usize {
        match self {
            Hits::None => 0,
            Hits::Rows { list, .. } => list.len(),
            Hits::Cells { list, .. } => list.len(),
            Hits::Text(list) => list.len(),
        }
    }
}

/// A row of the tree, shown or not, whose key or value holds the query.
struct RowHit {
    doc: usize,
    keys: Vec<String>,
    path: String,
    in_key: bool,
    in_value: bool,
}

impl PaneFind {
    /// Computes the hits again if needed. True when the query has changed:
    /// then the first hit is to be shown.
    fn update(&mut self, mode: ViewMode, fmt: FormatOptions, compute: impl FnOnce(&str) -> Hits) -> bool {
        let same = self.key.as_ref().is_some_and(|(q, m, f)| *q == self.query && *m == mode && *f == fmt);
        if same && !self.stale {
            return false;
        }
        let query_changed = !self.key.as_ref().is_some_and(|(q, ..)| *q == self.query);
        self.hits = compute(&self.query);
        self.key = Some((self.query.clone(), mode, fmt));
        self.stale = false;
        self.current = if query_changed { 0 } else { self.current.min(self.hits.len().saturating_sub(1)) };
        query_changed && self.hits.len() > 0
    }

    /// Highlight of a hit in a cell. Light, for the coloured value to stay
    /// readable: the current one has an outline.
    fn mark(&self, ui: &Ui, index: usize, theme: &Theme) {
        let stroke = if index == self.current { egui::Stroke::new(1.5, theme.warning) } else { egui::Stroke::NONE };
        ui.painter().rect(ui.max_rect(), 2, theme.warning.gamma_multiply(0.2), stroke, egui::StrokeKind::Inside);
    }

    fn row(&self, path: &str) -> Option<(usize, &RowHit)> {
        let Hits::Rows { list, by_path } = &self.hits else { return None };
        if !self.open {
            return None;
        }
        by_path.get(path).map(|&i| (i, &list[i]))
    }

    fn cell(&self, doc: usize, column: &str) -> Option<usize> {
        let Hits::Cells { by_cell, .. } = &self.hits else { return None };
        if !self.open || by_cell.is_empty() {
            return None;
        }
        by_cell.get(&(doc, column.to_string())).copied()
    }
}

/// Rows of the tree holding `query` (not case-sensitive), expanded or not.
fn find_rows(docs: &[Bson], query: &str, fmt: FormatOptions) -> Vec<RowHit> {
    struct Walk<'a> {
        needle: String,
        fmt: FormatOptions,
        out: Vec<RowHit>,
        keys: Vec<Cow<'a, str>>,
    }
    impl<'a> Walk<'a> {
        fn holds(&self, text: &str) -> bool {
            text.to_lowercase().contains(&self.needle)
        }

        fn visit(&mut self, v: &'a Bson, doc: usize, in_key: bool) {
            if self.out.len() >= MAX_MATCHES {
                return;
            }
            let in_value = match v {
                Bson::Document(_) | Bson::Array(_) => false,
                Bson::String(s) => self.holds(s),
                other => self.holds(&bsonfmt::short_value(other, self.fmt)),
            };
            if in_key || in_value {
                let keys: Vec<String> = self.keys.iter().map(|k| k.to_string()).collect();
                let mut path = doc.to_string();
                for k in &keys {
                    path.push('/');
                    path.push_str(k);
                }
                self.out.push(RowHit { doc, keys, path, in_key, in_value });
            }
            match v {
                Bson::Document(d) => {
                    for (k, child) in d {
                        let in_key = self.holds(k);
                        self.keys.push(Cow::Borrowed(k));
                        self.visit(child, doc, in_key);
                        self.keys.pop();
                    }
                }
                Bson::Array(a) => {
                    for (i, child) in a.iter().enumerate() {
                        self.keys.push(Cow::Owned(format!("[{i}]")));
                        self.visit(child, doc, false);
                        self.keys.pop();
                    }
                }
                _ => {}
            }
        }
    }
    if query.is_empty() {
        return Vec::new();
    }
    let mut walk = Walk { needle: query.to_lowercase(), fmt, out: Vec::new(), keys: Vec::new() };
    for (i, d) in docs.iter().enumerate() {
        walk.visit(d, i, false);
    }
    walk.out
}

fn row_hits(docs: &[Bson], query: &str, fmt: FormatOptions) -> Hits {
    let list = find_rows(docs, query, fmt);
    let by_path = list.iter().enumerate().map(|(i, h)| (h.path.clone(), i)).collect();
    Hits::Rows { list, by_path }
}

/// The table shows top-level values: a hit deep inside one is a hit on its cell.
fn cell_hits(docs: &[Bson], query: &str, fmt: FormatOptions) -> Hits {
    let mut list = Vec::new();
    let mut by_cell = HashMap::new();
    for h in find_rows(docs, query, fmt) {
        let cell = (h.doc, h.keys.into_iter().next().unwrap_or_else(|| "value".to_string()));
        by_cell.entry(cell.clone()).or_insert_with(|| {
            list.push(cell);
            list.len() - 1
        });
    }
    Hits::Cells { list, by_cell }
}

impl OutputPane {
    pub fn new(output: ShellOutput, mode: ViewMode, batch: u64, auto_expand: bool) -> Self {
        let (skip, page_size) = match &output.kind {
            OutputKind::Docs { spec: Some(s), .. } => {
                let size = if s.limit > 0 { (s.limit as u64).min(batch) } else { batch };
                (s.skip, size)
            }
            _ => (0, batch),
        };
        let mut expanded = HashSet::new();
        if auto_expand {
            expanded.insert("0".to_string());
        }
        OutputPane {
            output,
            mode,
            loading: false,
            skip,
            page_size,
            expanded,
            selected: Selection::default(),
            text_cache: None,
            folded: HashSet::new(),
            fold_cache: None,
            rows_cache: None,
            columns_cache: None,
            skip_input: skip.to_string(),
            size_input: page_size.to_string(),
            pointer_over: false,
            hovered: None,
            find: PaneFind::default(),
            cell_edit: None,
            menu_cell: None,
        }
    }

    pub fn docs(&self) -> &[Bson] {
        docs_of(&self.output)
    }

    /// Ctrl+F: an error message has nothing to search in.
    pub fn can_find(&self) -> bool {
        !matches!(self.output.kind, OutputKind::Error(_))
    }

    pub fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus = true;
    }

    pub fn find_is_open(&self) -> bool {
        self.find.open
    }

    /// Ctrl+C with the pointer on a value: this value, or the selection when
    /// the value is part of it. Ctrl+Shift+C (`names`) adds the names.
    pub fn hovered_text(&self, fmt: FormatOptions, names: bool) -> Option<String> {
        let h = self.hovered.as_ref()?;
        if self.selected.len() > 1 && self.selected.contains(&h.path) {
            return self.selected_text(fmt, names);
        }
        let value = lookup(self.docs(), h.doc, &h.keys)?;
        Some(if names { named_value(&h.keys, value, fmt) } else { copy_value(value, fmt) })
    }

    /// Ctrl+C on the selected rows, like the first entries of their menu: the
    /// values one per line, or the documents as soon as one is selected whole.
    /// Ctrl+Shift+C (`names`) adds the names to the values.
    pub fn selected_text(&self, fmt: FormatOptions, names: bool) -> Option<String> {
        let docs = self.docs();
        let mut indices: Vec<usize> = match self.mode {
            ViewMode::Text => return None,
            ViewMode::Table => self.selected.items.iter().filter_map(|p| p.parse().ok()).collect(),
            ViewMode::Tree => {
                let (_, _, rows) = self.rows_cache.as_ref()?;
                let selected: Vec<&Row> = rows.iter().filter(|r| self.selected.contains(&r.path)).collect();
                if !selected.is_empty() && selected.iter().all(|r| !r.keys.is_empty()) {
                    if names {
                        let values: Vec<(&[String], &Bson)> = selected.iter().filter_map(|r| Some((r.keys.as_slice(), lookup(docs, r.doc, &r.keys)?))).collect();
                        return Some(named_values_text(&values, fmt));
                    }
                    let values: Vec<&Bson> = selected.iter().filter_map(|r| lookup(docs, r.doc, &r.keys)).collect();
                    return Some(values_text(&values, fmt));
                }
                selected.iter().map(|r| r.doc).collect()
            }
        };
        indices.sort_unstable();
        indices.dedup();
        let roots: Vec<&Bson> = indices.iter().filter_map(|&i| docs.get(i)).collect();
        match roots.as_slice() {
            [] => None,
            [root] => Some(bsonfmt::to_shell_json(root, 0, fmt)),
            _ => Some(documents_text(&roots, fmt)),
        }
    }

    /// Shows the current hit: in the tree, its parents have to be expanded.
    fn go_to_hit(&mut self) {
        self.find.scroll = 3;
        let Hits::Rows { list, .. } = &self.find.hits else { return };
        let Some(hit) = list.get(self.find.current) else { return };
        let mut path = hit.doc.to_string();
        let mut opened = false;
        for k in &hit.keys {
            opened |= self.expanded.insert(path.clone());
            path.push('/');
            path.push_str(k);
        }
        if opened {
            self.rows_cache = None;
        }
    }

    pub fn set_page(&mut self, docs: Vec<Bson>, skip: u64) {
        if let OutputKind::Docs { docs: d, more, .. } = &mut self.output.kind {
            *more = docs.len() as u64 >= self.page_size;
            *d = docs;
        }
        self.skip = skip;
        self.skip_input = skip.to_string();
        self.text_cache = None;
        self.folded.clear();
        self.fold_cache = None;
        self.rows_cache = None;
        self.columns_cache = None;
        self.selected.clear();
        self.find.stale = true;
        self.cell_edit = None;
    }

    /// Why a value cannot be edited in place, if it cannot: only the values
    /// of the documents of a `find`, outside `_id`, that are not objects or
    /// arrays. A missing top-level field can be added.
    pub fn cell_editable(&self, doc: usize, keys: &[String]) -> Result<(), &'static str> {
        if self.source().is_none() {
            return Err("Only the results of a find can be edited");
        }
        let Some(d) = self.docs().get(doc).and_then(|d| d.as_document()) else { return Err("Not a document") };
        if !d.contains_key("_id") {
            return Err("The document has no _id");
        }
        let Some(first) = keys.first() else { return Err("Documents are edited with Edit Document…") };
        if first == "_id" {
            return Err("The _id of a document cannot change");
        }
        if keys.iter().any(|k| !is_index(k) && (k.contains('.') || k.starts_with('$'))) {
            return Err("Field names with a dot or a $ cannot be edited here");
        }
        match lookup(self.docs(), doc, keys) {
            Some(v) if bsonfmt::is_container(v) => Err(CONTAINER),
            None if keys.len() > 1 => Err("Not a value of the document"),
            _ => Ok(()),
        }
    }

    /// Starts editing a value, in shell syntax.
    pub fn edit_cell(&mut self, doc: usize, keys: &[String]) -> Result<(), &'static str> {
        self.cell_editable(doc, keys)?;
        // Dates in UTC, legacy UUIDs as is: the text must read back as the same value.
        let text = lookup(self.docs(), doc, keys).map(|v| bsonfmt::scalar_shell(v, FormatOptions::default())).unwrap_or_default();
        self.cell_edit = Some(CellEdit { doc, keys: keys.to_vec(), text, error: None, focused: false });
        Ok(())
    }

    /// The command that sets (or, with `None`, removes) a value of a
    /// document of the page, with its database.
    pub fn field_update(&self, doc: usize, keys: &[String], value: Option<Bson>) -> Option<(String, bson::Document)> {
        let (db, coll) = self.source()?;
        let id = self.docs().get(doc)?.as_document()?.get("_id")?.clone();
        let path = dotted_path(keys);
        let change = match value {
            Some(v) => doc! { "$set": { path: v } },
            None => doc! { "$unset": { path: "" } },
        };
        Some((db, doc! { "update": coll, "updates": [{ "q": { "_id": id }, "u": change, "upsert": false }] }))
    }

    /// Ends an edit with Enter: the update to send, if the value changed. A
    /// text that does not parse keeps the edit open, with the error.
    pub fn commit_cell(&mut self) -> Option<(String, bson::Document)> {
        let edit = self.cell_edit.take()?;
        if edit.text.trim().is_empty() {
            return None;
        }
        let value = match crate::shell::parse_shell_value(&edit.text) {
            Ok(v) => v,
            Err(e) => {
                // Enter took the focus away: give it back, to correct the text.
                self.cell_edit = Some(CellEdit { error: Some(e), focused: false, ..edit });
                return None;
            }
        };
        if lookup(self.docs(), edit.doc, &edit.keys) == Some(&value) {
            return None;
        }
        self.field_update(edit.doc, &edit.keys, Some(value))
    }

    fn source(&self) -> Option<(String, String)> {
        match &self.output.kind {
            OutputKind::Docs { spec: Some(s), .. } => Some((s.db.clone(), s.coll.clone())),
            _ => None,
        }
    }
}

/// Selected rows (tree paths, or document indices in the table view).
#[derive(Default)]
pub struct Selection {
    items: HashSet<String>,
    /// Start of Shift+click ranges.
    anchor: Option<String>,
    /// Last click, to know which result pane the Delete key applies to.
    pub touched: Option<std::time::Instant>,
}

impl Selection {
    pub fn contains(&self, path: &str) -> bool {
        self.items.contains(path)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.anchor = None;
    }

    /// Click on `path`: Ctrl toggles it, Shift selects the range from the
    /// last clicked row in the visible `order`, otherwise only `path`.
    pub fn click<I: IntoIterator<Item = String>>(&mut self, path: &str, mods: egui::Modifiers, order: impl FnOnce() -> I) {
        self.touched = Some(std::time::Instant::now());
        if mods.shift
            && let Some(anchor) = self.anchor.clone()
        {
            let mut range = Vec::new();
            let mut inside = false;
            for p in order() {
                let edge = p == anchor || p == path;
                if edge || inside {
                    range.push(p);
                }
                if edge && (inside || anchor == path) {
                    break;
                }
                inside |= edge;
            }
            // The anchor may be hidden now (collapsed): then a plain click.
            if range.contains(&anchor) {
                if !mods.command {
                    self.items.clear();
                }
                self.items.extend(range);
                return;
            }
        }
        if mods.command {
            if !self.items.remove(path) {
                self.items.insert(path.to_string());
            }
        } else {
            self.items.clear();
            self.items.insert(path.to_string());
        }
        self.anchor = Some(path.to_string());
    }

    /// Right click: keeps the selection when `path` is part of it.
    pub fn context(&mut self, path: &str) {
        if !self.contains(path) {
            self.click(path, egui::Modifiers::NONE, Vec::new);
        }
    }
}

pub struct PaneCtx<'a> {
    pub tab: u64,
    pub server: u64,
    pub index: usize,
    pub theme: &'a Theme,
    pub fmt: FormatOptions,
    pub font_size: f32,
    pub actions: &'a mut Vec<Action>,
}

fn docs_of(output: &ShellOutput) -> &[Bson] {
    match &output.kind {
        OutputKind::Docs { docs, .. } => docs,
        _ => &[],
    }
}

pub fn show_pane(ui: &mut Ui, pane: &mut OutputPane, cx: &mut PaneCtx<'_>) {
    pane.pointer_over = ui.rect_contains_pointer(ui.max_rect());
    header(ui, pane, cx);
    ui.add_space(2.0);
    if pane.find.open && pane.can_find() {
        find_row(ui, pane, cx);
    }
    // A jump: the row of a far cell has to be there at once, to scroll to the cell.
    if pane.find.scroll > 0 {
        ui.style_mut().scroll_animation = egui::style::ScrollAnimation::none();
    }
    match &pane.output.kind {
        OutputKind::Text(t) => {
            let t = t.clone();
            let mut find = None;
            if pane.find.open {
                if pane.find.update(pane.mode, cx.fmt, |q| Hits::Text(find_all(&t, q))) {
                    pane.go_to_hit();
                }
                find = text_matches(&pane.find);
            }
            crate::ui::editor::searchable_code_view(ui, Id::new(("out-text", cx.tab, cx.index)), &t, cx.theme, FontId::monospace(cx.font_size), find.as_ref());
        }
        OutputKind::Error(e) => {
            let e = e.clone();
            egui::ScrollArea::both().id_salt(("out-err", cx.tab, cx.index)).auto_shrink([false, false]).show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(e).color(cx.theme.error).font(FontId::monospace(cx.font_size)))
                        .selectable(true)
                        .wrap(),
                );
            });
        }
        OutputKind::Docs { .. } => {
            if pane.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading…");
                });
            }
            match pane.mode {
                ViewMode::Tree => tree_view(ui, pane, cx),
                ViewMode::Table => table_view(ui, pane, cx),
                ViewMode::Text => text_view(ui, pane, cx),
            }
        }
    }
    if pane.find.scroll > 0 {
        pane.find.scroll -= 1;
        ui.ctx().request_repaint();
    }
}

fn text_matches(find: &PaneFind) -> Option<TextMatches<'_>> {
    let Hits::Text(matches) = &find.hits else { return None };
    Some(TextMatches { matches, current: find.current, scroll: find.scroll > 0 })
}

/// The search bar of a pane. The tree and the table search the documents of
/// the page; the text views search their text, in `text_view` or `show_pane`.
fn find_row(ui: &mut Ui, pane: &mut OutputPane, cx: &mut PaneCtx<'_>) {
    let id = Id::new(("out-find", cx.tab, cx.index));
    let find = &mut pane.find;
    let escape = (find.had_focus || ui.memory(|m| m.has_focus(id))) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let bar = crate::ui::editor::find_bar(ui, id, &mut find.query, &mut find.focus, find.hits.len(), find.current, cx.theme);
    find.had_focus = bar.has_focus;
    if escape || bar.close {
        find.open = false;
        find.had_focus = false;
        return;
    }
    let mut go = false;
    if let Some(next) = bar.step {
        let n = find.hits.len();
        find.current = if next { (find.current + 1) % n } else { (find.current + n - 1) % n };
        go = true;
    }
    let (mode, fmt) = (pane.mode, cx.fmt);
    let docs = docs_of(&pane.output);
    match (&pane.output.kind, mode) {
        (OutputKind::Docs { .. }, ViewMode::Tree) => go |= find.update(mode, fmt, |q| row_hits(docs, q, fmt)),
        (OutputKind::Docs { .. }, ViewMode::Table) => go |= find.update(mode, fmt, |q| cell_hits(docs, q, fmt)),
        _ => {}
    }
    if go {
        pane.go_to_hit();
    }
}

fn header(ui: &mut Ui, pane: &mut OutputPane, cx: &mut PaneCtx<'_>) {
    let th = cx.theme;
    egui::Frame::new()
        .fill(th.surface)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (icon, title) = match &pane.output.kind {
                    OutputKind::Docs { spec: Some(s), .. } => (crate::ui::ICON_COLLECTION, s.coll.clone()),
                    OutputKind::Docs { ns: Some(ns), .. } => (crate::ui::ICON_COLLECTION, ns.clone()),
                    OutputKind::Docs { .. } => (icon::BRACKETS_CURLY, "Result".to_string()),
                    OutputKind::Text(_) => (icon::TEXT_ALIGN_LEFT, "Text".to_string()),
                    OutputKind::Error(_) => (icon::WARNING, "Error".to_string()),
                };
                let color = if matches!(pane.output.kind, OutputKind::Error(_)) { th.error } else { th.text };
                ui.label(RichText::new(format!("{icon} {title}")).strong().color(color))
                    .on_hover_text(&pane.output.statement);
                ui.label(RichText::new(format!("{:.3} sec.", pane.output.elapsed.as_secs_f64())).color(th.text_weak));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !matches!(pane.output.kind, OutputKind::Docs { .. }) {
                        return;
                    }
                    for (mode, glyph, name, tip) in [
                        (ViewMode::Text, icon::TEXT_ALIGN_LEFT, "Text", "Text mode (F4)"),
                        (ViewMode::Table, icon::TABLE, "Table", "Table mode (F3)"),
                        (ViewMode::Tree, icon::TREE_VIEW, "Tree", "Tree mode (F2)"),
                    ] {
                        if ui.selectable_label(pane.mode == mode, format!("{glyph} {name}")).on_hover_text(tip).clicked() {
                            pane.mode = mode;
                        }
                    }
                    ui.separator();
                    paging(ui, pane, cx);
                    if let OutputKind::Docs { spec: Some(spec), .. } = &pane.output.kind {
                        ui.separator();
                        if ui.small_button(format!("{} Export", crate::ui::ICON_EXPORT)).on_hover_text("Export the documents of this query").clicked() {
                            cx.actions.push(Action::Dialog(crate::ui::dialogs::Dialog::export_query(cx.server, spec)));
                        }
                    }
                });
            });
        });
}

fn paging(ui: &mut Ui, pane: &mut OutputPane, cx: &mut PaneCtx<'_>) {
    let OutputKind::Docs { docs, spec, more, .. } = &pane.output.kind else { return };
    let n = docs.len() as u64;
    if spec.is_none() {
        let txt = if *more { format!("first {n} documents") } else { format!("{n} document{}", if n == 1 { "" } else { "s" }) };
        ui.label(RichText::new(txt).color(cx.theme.text_weak));
        return;
    }
    let more = *more;
    // Right-to-left layout: widgets appear in reverse order.
    let next = ui.add_enabled(more && !pane.loading, egui::Button::new(icon::CARET_RIGHT).small()).on_hover_text("Next page");
    let size = ui.add(egui::TextEdit::singleline(&mut pane.size_input).desired_width(36.0)).on_hover_text("Page size (limit)");
    let skip = ui.add(egui::TextEdit::singleline(&mut pane.skip_input).desired_width(46.0)).on_hover_text("Skip");
    let prev = ui.add_enabled(pane.skip > 0 && !pane.loading, egui::Button::new(icon::CARET_LEFT).small()).on_hover_text("Previous page");
    let range = if n == 0 { "no documents".to_string() } else { format!("{} – {}", pane.skip + 1, pane.skip + n) };
    ui.label(RichText::new(range).color(cx.theme.text_weak));

    let page_size = pane.size_input.trim().parse::<u64>().unwrap_or(pane.page_size).clamp(1, 100_000);
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let mut target = None;
    if next.clicked() {
        target = Some(pane.skip + pane.page_size);
    }
    if prev.clicked() {
        target = Some(pane.skip.saturating_sub(page_size));
    }
    if (size.lost_focus() || skip.lost_focus()) && enter {
        target = Some(pane.skip_input.trim().parse().unwrap_or(pane.skip));
    }
    if let Some(skip) = target {
        pane.page_size = page_size;
        cx.actions.push(Action::Page { tab: cx.tab, out: cx.index, skip, limit: page_size });
    }
}

// ------------------------------------------------------------------ tree

struct Row {
    path: String,
    doc: usize,
    keys: Vec<String>,
    depth: usize,
    key: String,
    value: String,
    tname: &'static str,
    color: egui::Color32,
    container: bool,
}

fn flatten(docs: &[Bson], expanded: &HashSet<String>, fmt: FormatOptions, theme: &Theme) -> Vec<Row> {
    #[allow(clippy::too_many_arguments)]
    fn walk(out: &mut Vec<Row>, b: &Bson, path: String, doc: usize, keys: Vec<String>, depth: usize, expanded: &HashSet<String>, fmt: FormatOptions, theme: &Theme) {
        if !expanded.contains(&path) {
            return;
        }
        let children: Vec<(String, &Bson)> = match b {
            Bson::Document(d) => d.iter().map(|(k, v)| (k.clone(), v)).collect(),
            Bson::Array(a) => a.iter().enumerate().map(|(i, v)| (format!("[{i}]"), v)).collect(),
            _ => return,
        };
        for (k, v) in children {
            let p = format!("{path}/{k}");
            let mut ks = keys.clone();
            ks.push(k.clone());
            out.push(Row {
                path: p.clone(),
                doc,
                keys: ks.clone(),
                depth,
                key: k,
                value: bsonfmt::short_value(v, fmt),
                tname: bsonfmt::type_name(v),
                color: bson_color(theme, v),
                container: bsonfmt::is_container(v),
            });
            walk(out, v, p, doc, ks, depth + 1, expanded, fmt, theme);
        }
    }
    let mut out = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        let path = i.to_string();
        out.push(Row {
            path: path.clone(),
            doc: i,
            keys: Vec::new(),
            depth: 0,
            key: bsonfmt::root_label(i + 1, d, fmt),
            value: bsonfmt::short_value(d, fmt),
            tname: bsonfmt::type_name(d),
            color: bson_color(theme, d),
            container: bsonfmt::is_container(d),
        });
        walk(&mut out, d, path, i, Vec::new(), 1, expanded, fmt, theme);
    }
    out
}

fn lookup<'a>(docs: &'a [Bson], doc: usize, keys: &[String]) -> Option<&'a Bson> {
    let mut cur = docs.get(doc)?;
    for k in keys {
        cur = match cur {
            Bson::Document(d) => d.get(k)?,
            Bson::Array(a) => a.get(k.trim_start_matches('[').trim_end_matches(']').parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn expand_all(set: &mut HashSet<String>, b: &Bson, path: String) {
    match b {
        Bson::Document(d) => {
            for (k, v) in d {
                expand_all(set, v, format!("{path}/{k}"));
            }
        }
        Bson::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                expand_all(set, v, format!("{path}/[{i}]"));
            }
        }
        _ => return,
    }
    set.insert(path);
}

/// Single-line label of a result cell, cut at the width of the cell. Text that
/// reads from right to left is cut on the left, to show its start.
fn cell_label(ui: &mut Ui, text: &str, font: &FontId, color: egui::Color32) {
    use crate::bidi;
    if !bidi::has_rtl(text) {
        ui.add(egui::Label::new(RichText::new(text).font(font.clone()).color(color)).selectable(false).truncate());
        return;
    }
    let job = bidi::layout_job(text, font.clone(), color);
    if !bidi::starts_rtl(text) {
        ui.add(egui::Label::new(job).selectable(false).truncate());
        return;
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let rect = ui.available_rect_before_wrap();
    if galley.size().x <= rect.width() {
        ui.add(egui::Label::new(galley).selectable(false));
    } else {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.shrink_clip_rect(rect);
            ui.add(egui::Label::new(galley).selectable(false));
        });
    }
}

fn tree_view(ui: &mut Ui, pane: &mut OutputPane, cx: &mut PaneCtx<'_>) {
    let rows = match pane.rows_cache.take() {
        Some((f, t, rows)) if f == cx.fmt && t == cx.theme.name => rows,
        _ => flatten(pane.docs(), &pane.expanded, cx.fmt, cx.theme),
    };
    let row_h = cx.font_size + 8.0;
    let mut toggle: Option<String> = None;
    let mut menu_actions = Vec::new();
    let th = cx.theme;
    let font = FontId::proportional(cx.font_size);
    let mono = FontId::monospace(cx.font_size);
    let indent = cx.font_size * 1.2;
    let mut hovered = None;
    // The value being edited, out of the pane while the rows borrow it.
    let mut edit = pane.cell_edit.take();
    let (mut edit_drawn, mut edit_end, mut start_edit) = (false, None, None);
    let scroll_row = match &pane.find.hits {
        Hits::Rows { list, .. } if pane.find.open && pane.find.scroll > 0 => list.get(pane.find.current).and_then(|h| rows.iter().position(|r| r.path == h.path)),
        _ => None,
    };

    let mut table = TableBuilder::new(ui)
        .id_salt(("tree", cx.tab, cx.index))
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::initial(280.0).at_least(80.0).clip(true))
        .column(Column::initial(320.0).at_least(80.0).clip(true))
        .column(Column::remainder().at_least(70.0).clip(true))
        .auto_shrink([false, false]);
    if let Some(row) = scroll_row {
        table = table.scroll_to_row(row, Some(Align::Center));
    }
    table
        .header(row_h, |mut h| {
            for t in ["Key", "Value", "Type"] {
                h.col(|ui| {
                    ui.strong(t);
                });
            }
        })
        .body(|body| {
            body.rows(row_h, rows.len(), |mut row| {
                let r = &rows[row.index()];
                row.set_selected(pane.selected.contains(&r.path));
                let hit = pane.find.row(&r.path);
                row.col(|ui| {
                    if let Some((i, _)) = hit.filter(|(_, h)| h.in_key) {
                        pane.find.mark(ui, i, th);
                    }
                    ui.add_space(indent * r.depth as f32);
                    if r.container {
                        let open = pane.expanded.contains(&r.path);
                        let arrow = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
                        if ui.add(egui::Button::new(RichText::new(arrow).color(th.text_weak)).frame(false).small()).clicked() {
                            toggle = Some(r.path.clone());
                        }
                    } else {
                        ui.add_space(indent);
                    }
                    let key_color = if r.depth == 0 { th.text } else { th.syntax.property };
                    cell_label(ui, &r.key, &font, key_color);
                });
                let editing = edit.as_mut().filter(|e| e.doc == r.doc && e.keys == r.keys);
                row.col(|ui| {
                    if let Some(e) = editing {
                        edit_drawn = true;
                        if let Some(end) = cell_editor(ui, e, &mono, th) {
                            edit_end = Some(end);
                        }
                        return;
                    }
                    if let Some((i, _)) = hit.filter(|(_, h)| h.in_value) {
                        pane.find.mark(ui, i, th);
                    }
                    cell_label(ui, &r.value, &mono, r.color);
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new(r.tname).color(th.text_weak)).selectable(false).truncate());
                });
                let resp = row.response();
                if resp.contains_pointer() {
                    hovered = Some(HoveredValue { doc: r.doc, keys: r.keys.clone(), path: r.path.clone() });
                }
                if resp.clicked() {
                    let mods = resp.ctx.input(|i| i.modifiers);
                    pane.selected.click(&r.path, mods, || rows.iter().map(|r| r.path.clone()));
                }
                if resp.secondary_clicked() {
                    pane.selected.context(&r.path);
                }
                if resp.double_clicked() {
                    if r.container {
                        toggle = Some(r.path.clone());
                    } else {
                        start_edit = Some((r.doc, r.keys.clone()));
                    }
                }
                resp.context_menu(|ui| {
                    let targets: Vec<Target<'_>> = if pane.selected.len() > 1 && pane.selected.contains(&r.path) {
                        rows.iter().filter(|r| pane.selected.contains(&r.path)).map(|r| Target { doc: r.doc, keys: Some(&r.keys), path: Some(&r.path) }).collect()
                    } else {
                        vec![Target { doc: r.doc, keys: Some(&r.keys), path: Some(&r.path) }]
                    };
                    if let [_] = targets.as_slice()
                        && !r.keys.is_empty()
                    {
                        cell_menu(ui, pane, cx_parts(cx), r.doc, &r.keys, &mut menu_actions);
                    }
                    doc_menu(ui, pane, cx_parts(cx), &targets, &mut menu_actions);
                });
            });
        });

    pane.hovered = hovered;
    // Scrolled out of sight or folded: the edit is dropped, like a click elsewhere.
    pane.cell_edit = edit.filter(|_| edit_drawn);
    end_cell_edit(pane, cx, edit_end, start_edit);
    let expansion_changed = toggle.is_some() || !menu_actions.is_empty();
    if let Some(p) = toggle
        && !pane.expanded.remove(&p)
    {
        pane.expanded.insert(p);
    }
    apply_menu_actions(pane, cx, menu_actions);
    if !expansion_changed {
        pane.rows_cache = Some((cx.fmt, cx.theme.name, rows));
    }
}

/// Copy of the values of `PaneCtx` needed inside menus (avoids borrowing
/// `actions` twice).
#[derive(Clone, Copy)]
struct MenuCx {
    tab: u64,
    /// Index of the result pane in its tab.
    index: usize,
    server: u64,
    fmt: FormatOptions,
}

fn cx_parts(cx: &PaneCtx<'_>) -> MenuCx {
    MenuCx { tab: cx.tab, index: cx.index, server: cx.server, fmt: cx.fmt }
}

#[allow(clippy::large_enum_variant)] // built only on menu clicks
enum MenuAction {
    Push(Action),
    EditCell(usize, Vec<String>),
    ExpandAll(String),
    CollapseAll(String),
}

fn apply_menu_actions(pane: &mut OutputPane, cx: &mut PaneCtx<'_>, list: Vec<MenuAction>) {
    for a in list {
        match a {
            MenuAction::Push(a) => cx.actions.push(a),
            MenuAction::EditCell(doc, keys) => {
                let _ = pane.edit_cell(doc, &keys);
            }
            MenuAction::ExpandAll(path) => {
                let mut parts = path.split('/');
                let doc: usize = parts.next().and_then(|d| d.parse().ok()).unwrap_or(0);
                let keys: Vec<String> = parts.map(String::from).collect();
                if let Some(b) = lookup(pane.docs(), doc, &keys).cloned() {
                    expand_all(&mut pane.expanded, &b, path);
                }
            }
            MenuAction::CollapseAll(path) => {
                let prefix = format!("{path}/");
                pane.expanded.retain(|p| p != &path && !p.starts_with(&prefix));
            }
        }
    }
}

/// Row a context menu applies to: a document, or a value inside it (tree).
struct Target<'a> {
    doc: usize,
    keys: Option<&'a [String]>,
    path: Option<&'a str>,
}

fn doc_menu(ui: &mut Ui, pane: &OutputPane, cx: MenuCx, targets: &[Target<'_>], out: &mut Vec<MenuAction>) {
    match targets {
        [t] => single_menu(ui, pane, cx, t.doc, t.keys, t.path, out),
        _ => multi_menu(ui, pane, cx, targets, out),
    }
}

/// Menu of several selected rows: only the actions that apply to all.
fn multi_menu(ui: &mut Ui, pane: &OutputPane, cx: MenuCx, targets: &[Target<'_>], out: &mut Vec<MenuAction>) {
    let docs = pane.docs();
    let values: Vec<(&Target<'_>, &Bson)> = targets.iter().filter_map(|t| Some((t, lookup(docs, t.doc, t.keys.unwrap_or(&[]))?))).collect();
    let containers: Vec<&str> = values.iter().filter(|(_, v)| bsonfmt::is_container(v)).filter_map(|(t, _)| t.path).collect();
    if !containers.is_empty() {
        if menu::button(ui, "Expand Recursively").clicked() {
            out.extend(containers.iter().map(|p| MenuAction::ExpandAll(p.to_string())));
            ui.close();
        }
        if menu::button(ui, "Collapse Recursively").clicked() {
            out.extend(containers.iter().map(|p| MenuAction::CollapseAll(p.to_string())));
            ui.close();
        }
        ui.separator();
    }
    // Values inside documents (not the documents themselves): one per line.
    if values.iter().all(|(t, _)| t.keys.is_some_and(|k| !k.is_empty())) {
        if menu::entry(ui, true, "Copy Values", "Ctrl+C").clicked() {
            ui.ctx().copy_text(values_text(&values.iter().map(|(_, v)| *v).collect::<Vec<_>>(), cx.fmt));
            ui.close();
        }
        if menu::button(ui, "Copy Names").clicked() {
            ui.ctx().copy_text(values.iter().filter_map(|(t, _)| t.keys?.last().cloned()).collect::<Vec<_>>().join("\n"));
            ui.close();
        }
        if menu::button(ui, "Copy Paths").clicked() {
            ui.ctx().copy_text(values.iter().filter_map(|(t, _)| t.keys.map(dotted_path)).collect::<Vec<_>>().join("\n"));
            ui.close();
        }
        if menu::entry(ui, true, "Copy Names and Values", "Ctrl+Shift+C").clicked() {
            let named: Vec<(&[String], &Bson)> = values.iter().filter_map(|(t, v)| Some((t.keys?, *v))).collect();
            ui.ctx().copy_text(named_values_text(&named, cx.fmt));
            ui.close();
        }
        ui.separator();
    }

    // The documents of the selected rows, once each, as an array.
    let mut indices: Vec<usize> = targets.iter().map(|t| t.doc).collect();
    indices.dedup();
    let roots: Vec<&Bson> = indices.iter().filter_map(|&i| docs.get(i)).collect();
    let n = roots.len();
    let plural = if n == 1 { "" } else { "s" };
    if menu::button(ui, &format!("Copy JSON ({n} document{plural})")).clicked() {
        ui.ctx().copy_text(documents_text(&roots, cx.fmt));
        ui.close();
    }
    if menu::button(ui, &format!("Copy Extended JSON ({n} document{plural})")).clicked() {
        let v: Vec<serde_json::Value> = roots.iter().map(|&b| b.clone().into_relaxed_extjson()).collect();
        ui.ctx().copy_text(serde_json::to_string_pretty(&v).unwrap_or_default());
        ui.close();
    }
    // Deleting needs whole documents selected, not values inside them.
    let whole = targets.iter().all(|t| t.keys.is_none_or(|k| k.is_empty()));
    if whole && let Some((n, action)) = delete_documents(pane, cx.server, cx.tab, &indices, cx.fmt) {
        ui.separator();
        let plural = if n == 1 { "" } else { "s" };
        if menu::entry(ui, true, &format!("Delete {n} Document{plural}…"), "Del").clicked() {
            out.push(MenuAction::Push(action));
            ui.close();
        }
    }
}

/// Confirmation dialog deleting the documents at `indices` of the page, by
/// `_id`, with their count. `None` when the results do not come from a
/// collection or have no `_id`.
fn delete_documents(pane: &OutputPane, server: u64, tab: u64, indices: &[usize], fmt: FormatOptions) -> Option<(usize, Action)> {
    let (db, coll) = pane.source()?;
    let ids: Vec<Bson> = indices.iter().filter_map(|&i| pane.docs().get(i)?.as_document()?.get("_id").cloned()).collect();
    let (title, message, filter, label) = match ids.as_slice() {
        [] => return None,
        [id] => {
            let label = bsonfmt::scalar_shell(id, fmt);
            ("Delete Document", format!("Delete document with _id {label} from \"{coll}\"?"), doc! { "q": { "_id": id }, "limit": 1 }, format!("Delete document {label}"))
        }
        _ => {
            let n = ids.len();
            ("Delete Documents", format!("Delete {n} documents from \"{coll}\"?"), doc! { "q": { "_id": { "$in": &ids } }, "limit": 0 }, format!("Delete {n} documents"))
        }
    };
    let action = Action::Command { server, db, cmd: doc! { "delete": &coll, "deletes": [filter] }, label, refresh: Refresh::Tab(tab) };
    Some((ids.len(), Action::Dialog(Dialog::confirm(title, message, action))))
}

/// Delete key: deletes the selected documents (not values inside them).
/// `Err` explains why nothing can be deleted.
pub fn delete_selection(pane: &OutputPane, server: u64, tab: u64, fmt: FormatOptions) -> Result<Action, &'static str> {
    if pane.mode == ViewMode::Text {
        return Err("Select documents in the tree or table view to delete them");
    }
    if pane.selected.items.iter().any(|p| p.contains('/')) {
        return Err("Only whole documents can be deleted: the selection contains fields");
    }
    let mut indices: Vec<usize> = pane.selected.items.iter().filter_map(|p| p.parse().ok()).collect();
    indices.sort_unstable();
    delete_documents(pane, server, tab, &indices, fmt).map(|(_, a)| a).ok_or("These results cannot be deleted: they do not come from a collection or have no _id")
}

fn copy_value(v: &Bson, fmt: FormatOptions) -> String {
    match v {
        Bson::String(s) => s.clone(),
        other => bsonfmt::to_shell_json(other, 0, fmt),
    }
}

fn values_text(values: &[&Bson], fmt: FormatOptions) -> String {
    values.iter().map(|v| copy_value(v, fmt)).collect::<Vec<_>>().join("\n")
}

/// `"name" : value`, a line of its document; the document itself at its root.
fn named_value(keys: &[String], v: &Bson, fmt: FormatOptions) -> String {
    let value = bsonfmt::to_shell_json(v, 0, fmt);
    match keys {
        [.., name] => format!("{} : {value}", bsonfmt::js_string(&dotted_path(std::slice::from_ref(name)))),
        [] => value,
    }
}

/// With commas, to go inside `{ }` as it is.
fn named_values_text(values: &[(&[String], &Bson)], fmt: FormatOptions) -> String {
    values.iter().map(|(keys, v)| named_value(keys, v, fmt)).collect::<Vec<_>>().join(",\n")
}

fn documents_text(roots: &[&Bson], fmt: FormatOptions) -> String {
    bsonfmt::to_shell_json(&Bson::Array(roots.iter().map(|&b| b.clone()).collect()), 0, fmt)
}

/// `a.b.0.c` from tree keys (`[0]` for array elements).
/// A key of an array element in the tree: `[2]`.
fn is_index(key: &str) -> bool {
    key.strip_prefix('[').and_then(|k| k.strip_suffix(']')).is_some_and(|k| k.parse::<usize>().is_ok())
}

fn dotted_path(keys: &[String]) -> String {
    keys.iter().map(|k| k.strip_prefix('[').and_then(|k| k.strip_suffix(']')).unwrap_or(k)).collect::<Vec<_>>().join(".")
}

fn single_menu(ui: &mut Ui, pane: &OutputPane, cx: MenuCx, doc: usize, keys: Option<&[String]>, path: Option<&str>, out: &mut Vec<MenuAction>) {
    let docs = pane.docs();
    let Some(root) = docs.get(doc) else { return };
    let source = pane.source();
    let id = root.as_document().and_then(|d| d.get("_id")).cloned();

    if let (Some(path), Some(keys)) = (path, keys) {
        let value = lookup(docs, doc, keys);
        if value.is_some_and(bsonfmt::is_container) {
            if menu::button(ui, "Expand Recursively").clicked() {
                out.push(MenuAction::ExpandAll(path.to_string()));
                ui.close();
            }
            if menu::button(ui, "Collapse Recursively").clicked() {
                out.push(MenuAction::CollapseAll(path.to_string()));
                ui.close();
            }
            ui.separator();
        }
        if !keys.is_empty()
            && let Some(v) = value {
                if menu::entry(ui, true, "Copy Value", "Ctrl+C").clicked() {
                    ui.ctx().copy_text(copy_value(v, cx.fmt));
                    ui.close();
                }
                if menu::button(ui, "Copy Name").clicked() {
                    ui.ctx().copy_text(keys.last().cloned().unwrap_or_default());
                    ui.close();
                }
                if menu::button(ui, "Copy Path").clicked() {
                    ui.ctx().copy_text(dotted_path(keys));
                    ui.close();
                }
                if menu::entry(ui, true, "Copy Name and Value", "Ctrl+Shift+C").clicked() {
                    ui.ctx().copy_text(named_value(keys, v, cx.fmt));
                    ui.close();
                }
                ui.separator();
            }
    }

    if menu::button(ui, "Copy JSON").clicked() {
        ui.ctx().copy_text(bsonfmt::to_shell_json(root, 0, cx.fmt));
        ui.close();
    }
    if menu::button(ui, "Copy Extended JSON (relaxed)").clicked() {
        let v = root.clone().into_relaxed_extjson();
        ui.ctx().copy_text(serde_json::to_string_pretty(&v).unwrap_or_default());
        ui.close();
    }
    ui.separator();
    let text = bsonfmt::to_shell_json(root, 0, cx.fmt);
    let (db, coll) = source.clone().unwrap_or_default();
    if menu::button(ui, "View Document…").clicked() {
        out.push(MenuAction::Push(Action::Dialog(Dialog::document(DocMode::View, cx.server, &db, &coll, text.clone(), None, None))));
        ui.close();
    }
    let editable = source.is_some() && id.is_some();
    if menu::entry(ui, editable, "Edit Document…", "").clicked() {
        out.push(MenuAction::Push(Action::Dialog(Dialog::document(DocMode::Edit, cx.server, &db, &coll, text.clone(), id.clone(), Some(cx.tab)))));
        ui.close();
    }
    if menu::entry(ui, source.is_some(), "Insert Document…", "").clicked() {
        out.push(MenuAction::Push(Action::Dialog(Dialog::document(DocMode::Insert, cx.server, &db, &coll, "{\n    \n}".into(), None, Some(cx.tab)))));
        ui.close();
    }
    let delete = delete_documents(pane, cx.server, cx.tab, &[doc], cx.fmt);
    if menu::entry(ui, delete.is_some(), "Delete Document…", "Del").clicked()
        && let Some((_, action)) = delete
    {
        out.push(MenuAction::Push(action));
        ui.close();
    }
}

// ----------------------------------------------------------------- table

/// Union of top-level keys in order of appearance (capped), or a single
/// "value" column when the results are not documents.
fn table_columns(docs: &[Bson]) -> (Vec<String>, bool) {
    let mut columns: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for d in docs {
        if let Bson::Document(d) = d {
            for k in d.keys() {
                if columns.len() < 300 && seen.insert(k.as_str()) {
                    columns.push(k.clone());
                }
            }
        }
    }
    if columns.is_empty() {
        return (vec!["value".into()], true);
    }
    (columns, false)
}

fn table_view(ui: &mut Ui, pane: &mut OutputPane, cx: &mut PaneCtx<'_>) {
    if pane.columns_cache.is_none() {
        pane.columns_cache = Some(table_columns(pane.docs()));
    }
    let (columns, scalar_mode) = pane.columns_cache.take().unwrap_or_default();
    // The cell being edited, out of the pane while the table borrows it.
    let mut edit = pane.cell_edit.take();
    let docs = pane.docs();
    let row_h = cx.font_size + 8.0;
    let th = cx.theme;
    let mono = FontId::monospace(cx.font_size);
    let header_font = FontId::proportional(cx.font_size);
    let fmt = cx.fmt;
    let menu = cx_parts(cx);
    let mut menu_actions = Vec::new();
    let mut click: Option<(String, Option<egui::Modifiers>)> = None;
    let mut hovered = None;
    let find = &pane.find;
    let scrolling_to = match &find.hits {
        Hits::Cells { list, .. } if find.open && find.scroll > 0 => list.get(find.current),
        _ => None,
    };
    let mut cell_to_show = None;
    let mut edit_drawn = false;
    let mut edit_end: Option<bool> = None;
    let mut start_edit: Option<(usize, Vec<String>)> = None;
    let mut menu_cell = pane.menu_cell.clone();

    egui::ScrollArea::horizontal().id_salt(("table-h", cx.tab, cx.index)).show(ui, |ui| {
        let mut tb = TableBuilder::new(ui)
            .id_salt(("table", cx.tab, cx.index))
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .auto_shrink([false, false])
            .column(Column::exact(44.0));
        for _ in &columns {
            tb = tb.column(Column::initial(160.0).at_least(40.0).clip(true));
        }
        if let Some((doc, _)) = scrolling_to {
            tb = tb.scroll_to_row(*doc, Some(Align::Center));
        }
        tb.header(row_h, |mut h| {
            h.col(|ui| {
                ui.strong("#");
            });
            for c in &columns {
                h.col(|ui| {
                    if crate::bidi::has_rtl(c) {
                        let color = ui.visuals().strong_text_color();
                        cell_label(ui, c, &header_font, color);
                    } else {
                        ui.strong(c);
                    }
                });
            }
        })
        .body(|body| {
            body.rows(row_h, docs.len(), |mut row| {
                let i = row.index();
                let d = &docs[i];
                let path = i.to_string();
                row.set_selected(pane.selected.contains(&path));
                let (_, number) = row.col(|ui| {
                    ui.label(RichText::new((pane.skip + i as u64 + 1).to_string()).color(th.text_weak));
                });
                if number.contains_pointer() {
                    hovered = Some(HoveredValue { doc: i, keys: Vec::new(), path: path.clone() });
                }
                // Column under the pointer: clicks go to the row, not to its cells.
                let mut pointed = None;
                for c in &columns {
                    let v = if scalar_mode { Some(d) } else { d.as_document().and_then(|doc| doc.get(c)) };
                    let editing = edit.as_mut().filter(|e| e.doc == i && e.keys.len() == 1 && &e.keys[0] == c);
                    let (rect, cell) = row.col(|ui| {
                        if let Some(e) = editing {
                            edit_drawn = true;
                            if let Some(end) = cell_editor(ui, e, &mono, th) {
                                edit_end = Some(end);
                            }
                            return;
                        }
                        if let Some(hit) = find.cell(i, c) {
                            find.mark(ui, hit, th);
                        }
                        if let Some(v) = v {
                            cell_label(ui, &bsonfmt::short_value(v, fmt), &mono, bson_color(th, v));
                        }
                    });
                    if !scalar_mode && cell.contains_pointer() {
                        pointed = Some(c.clone());
                    }
                    if scrolling_to.is_some_and(|(doc, column)| *doc == i && column == c) {
                        cell_to_show = Some(rect);
                    }
                    if v.is_some() && cell.contains_pointer() {
                        let keys = if scalar_mode { Vec::new() } else { vec![c.clone()] };
                        hovered = Some(HoveredValue { doc: i, keys, path: path.clone() });
                    }
                }
                let resp = row.response();
                if let Some(c) = pointed {
                    if resp.double_clicked() {
                        start_edit = Some((i, vec![c.clone()]));
                    }
                    if resp.secondary_clicked() {
                        menu_cell = Some((i, c));
                    }
                }
                if resp.clicked() {
                    click = Some((path.clone(), Some(resp.ctx.input(|i| i.modifiers))));
                }
                if resp.secondary_clicked() {
                    click = Some((path.clone(), None));
                }
                resp.context_menu(|ui| {
                    let targets: Vec<Target<'_>> = if pane.selected.len() > 1 && pane.selected.contains(&path) {
                        (0..docs.len()).filter(|j| pane.selected.contains(&j.to_string())).map(|doc| Target { doc, keys: None, path: None }).collect()
                    } else {
                        vec![Target { doc: i, keys: None, path: None }]
                    };
                    if let [_] = targets.as_slice()
                        && let Some((_, column)) = menu_cell.as_ref().filter(|(doc, _)| *doc == i)
                    {
                        cell_menu(ui, pane, menu, i, std::slice::from_ref(column), &mut menu_actions);
                    }
                    doc_menu(ui, pane, menu, &targets, &mut menu_actions);
                });
            });
        });
        // Out of the table: its own scroll area would swallow the request.
        if let Some(rect) = cell_to_show {
            ui.scroll_to_rect(rect, None);
        }
    });
    pane.hovered = hovered;
    pane.menu_cell = menu_cell;
    // Scrolled out of sight: the edit is dropped, like a click elsewhere.
    pane.cell_edit = edit.filter(|_| edit_drawn);
    end_cell_edit(pane, cx, edit_end, start_edit);
    match click {
        Some((path, Some(mods))) => {
            let n = pane.docs().len();
            pane.selected.click(&path, mods, || (0..n).map(|i| i.to_string()));
        }
        Some((path, None)) => pane.selected.context(&path),
        None => {}
    }
    apply_menu_actions(pane, cx, menu_actions);
    if pane.columns_cache.is_none() {
        pane.columns_cache = Some((columns, scalar_mode));
    }
}

/// Ends the edit of a value (Enter saves it, Escape or a click elsewhere
/// drops it), then starts the one asked by a double-click.
fn end_cell_edit(pane: &mut OutputPane, cx: &mut PaneCtx<'_>, end: Option<bool>, start: Option<(usize, Vec<String>)>) {
    match end {
        Some(true) => {
            let path = pane.cell_edit.as_ref().map(|e| dotted_path(&e.keys)).unwrap_or_default();
            if let Some((db, cmd)) = pane.commit_cell() {
                cx.actions.push(Action::Command { server: cx.server, db, cmd, label: format!("Update field {path}"), refresh: Refresh::Page { tab: cx.tab, out: cx.index } });
            }
        }
        Some(false) => pane.cell_edit = None,
        None => {}
    }
    if let Some((doc, keys)) = start {
        let _ = pane.edit_cell(doc, &keys);
    }
}

/// The text field of a cell being edited. `Some(true)` on Enter,
/// `Some(false)` on Escape or a click elsewhere.
fn cell_editor(ui: &mut Ui, e: &mut CellEdit, font: &FontId, th: &Theme) -> Option<bool> {
    let r = ui.add(egui::TextEdit::singleline(&mut e.text).font(font.clone()).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(2, 0)));
    if e.error.is_some() {
        ui.painter().rect_stroke(r.rect, 2, egui::Stroke::new(1.5, th.error), egui::StrokeKind::Outside);
    }
    let r = match &e.error {
        Some(err) => r.on_hover_text(RichText::new(err).color(th.error)),
        None => r.on_hover_text("Shell syntax: \"text\", 42, NumberLong(1), ISODate(\"…\"), true, null.\nEnter saves, Escape cancels."),
    };
    if !e.focused {
        r.request_focus();
        e.focused = true;
        return None;
    }
    if r.changed() {
        e.error = None;
    }
    if r.lost_focus() {
        return Some(ui.input(|i| i.key_pressed(egui::Key::Enter)));
    }
    None
}

/// Items of the context menu for a value: a cell of the table, a row of the tree.
fn cell_menu(ui: &mut Ui, pane: &OutputPane, cx: MenuCx, doc: usize, keys: &[String], out: &mut Vec<MenuAction>) {
    let name = dotted_path(keys);
    let editable = pane.cell_editable(doc, keys);
    let r = menu::entry(ui, editable.is_ok(), &format!("Edit Value of \"{name}\""), "Double-click");
    if r.clicked() {
        out.push(MenuAction::EditCell(doc, keys.to_vec()));
        ui.close();
    }
    if let Err(why) = editable {
        r.on_disabled_hover_text(why);
    }
    // Objects and arrays too; not array elements, which $unset sets to null.
    let unset = matches!(editable, Ok(()) | Err(CONTAINER))
        && lookup(pane.docs(), doc, keys).is_some()
        && !keys.last().is_some_and(|k| is_index(k));
    if menu::entry(ui, unset, &format!("Remove Field \"{name}\"…"), "").clicked()
        && let Some((db, cmd)) = pane.field_update(doc, keys, None)
    {
        out.push(MenuAction::Push(Action::Dialog(Dialog::confirm(
            "Remove Field",
            format!("Remove the field \"{name}\" from this document?"),
            Action::Command { server: cx.server, db, cmd, label: format!("Remove field {name}"), refresh: Refresh::Page { tab: cx.tab, out: cx.index } },
        ))));
        ui.close();
    }
    ui.separator();
}

// ------------------------------------------------------------------ text

const TEXT_VIEW_LIMIT: usize = 2 << 20;

fn text_view(ui: &mut Ui, pane: &mut OutputPane, cx: &mut PaneCtx<'_>) {
    let fmt = cx.fmt;
    let needs = !matches!(&pane.text_cache, Some((f, _)) if *f == fmt);
    if needs {
        let mut s = String::new();
        let docs = pane.docs();
        for (i, d) in docs.iter().enumerate() {
            // Laying out megabytes of text freezes the UI: stop at TEXT_VIEW_LIMIT.
            if s.len() > TEXT_VIEW_LIMIT {
                s.push_str(&format!(
                    "/* {} more document(s) not shown: the text view is limited to {} MB. Use the tree view or a smaller page size. */\n",
                    docs.len() - i,
                    TEXT_VIEW_LIMIT >> 20
                ));
                break;
            }
            if docs.len() > 1 {
                s.push_str(&format!("/* {} */\n", pane.skip as usize + i + 1));
            }
            s.push_str(&bsonfmt::to_shell_json(d, 0, fmt));
            s.push_str("\n\n");
        }
        pane.text_cache = Some((fmt, s));
        pane.folded.clear();
        pane.fold_cache = None;
    }
    let Some((_, text)) = &pane.text_cache else { return };
    let view = pane.fold_cache.get_or_insert_with(|| fold_text(text, &pane.folded));
    if pane.find.open && pane.find.update(pane.mode, fmt, |q| Hits::Text(find_all(&view.text, q))) {
        pane.find.scroll = 3;
    }
    let find = if pane.find.open { text_matches(&pane.find) } else { None };
    let click = crate::ui::editor::folding_code_view(ui, Id::new(("out-json", cx.tab, cx.index)), &view.text, &view.marks, cx.theme, FontId::monospace(cx.font_size), find.as_ref());
    match click {
        Some(FoldClick::Toggle(row)) => {
            let line = view.lines[row];
            if !pane.folded.remove(&line) {
                pane.folded.insert(line);
            }
        }
        Some(FoldClick::CollapseAll) => pane.folded = view.collapse_all(),
        Some(FoldClick::ExpandAll) => pane.folded.clear(),
        None => return,
    }
    pane.fold_cache = None;
    pane.find.stale = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::shell::{QuerySpec, ShellOutput};
    use std::time::Duration;

    fn docs() -> Vec<Bson> {
        vec![
            Bson::Document(doc! { "_id": 1, "a": { "b": [1, { "c": 2 }] }, "s": "x" }),
            Bson::Document(doc! { "_id": 2, "z": true }),
        ]
    }

    fn pane(docs: Vec<Bson>, spec: Option<QuerySpec>) -> OutputPane {
        OutputPane::new(
            ShellOutput { statement: "q".into(), line: 0, kind: OutputKind::Docs { docs, spec, ns: None, more: true }, elapsed: Duration::ZERO },
            ViewMode::Tree,
            50,
            true,
        )
    }

    fn paths(hits: &[RowHit]) -> Vec<&str> {
        hits.iter().map(|h| h.path.as_str()).collect()
    }

    #[test]
    fn find_rows_searches_keys_and_values_everywhere() {
        let fmt = FormatOptions::default();
        let docs = vec![
            Bson::Document(doc! { "_id": 1, "name": "Ada LOVELACE", "tags": ["math", { "name": "x" }], "age": 36 }),
            Bson::Document(doc! { "_id": 2, "deep": { "deeper": { "NAME": "love" } } }),
        ];
        let hits = find_rows(&docs, "love", fmt);
        assert_eq!(paths(&hits), ["0/name", "1/deep/deeper/NAME"], "not case-sensitive, collapsed rows too");
        assert_eq!((hits[0].in_key, hits[0].in_value), (false, true));
        assert_eq!(hits[1].keys, ["deep", "deeper", "NAME"]);

        let hits = find_rows(&docs, "name", fmt);
        assert_eq!(paths(&hits), ["0/name", "0/tags/[1]/name", "1/deep/deeper/NAME"]);
        assert_eq!((hits[0].in_key, hits[0].in_value), (true, false));

        // Numbers as shown, and a value that is also in its key.
        assert_eq!(paths(&find_rows(&docs, "36", fmt)), ["0/age"]);
        // Not the position of an array element.
        assert!(find_rows(&[Bson::Document(doc! { "a": ["x"] })], "0", fmt).is_empty());
        // Not the summary of a container.
        assert!(find_rows(&docs, "elements", fmt).is_empty());
        assert!(find_rows(&docs, "", fmt).is_empty());
        // Results that are not documents.
        assert_eq!(paths(&find_rows(&[Bson::String("Plain".into())], "plain", fmt)), ["0"]);
        // A long string is searched beyond what the view shows.
        let long = format!("{}needle", "x".repeat(1000));
        assert_eq!(paths(&find_rows(&[Bson::Document(doc! { "s": long })], "needle", fmt)), ["0/s"]);
    }

    #[test]
    fn find_rows_stops_at_the_limit() {
        let docs: Vec<Bson> = (0..MAX_MATCHES + 50).map(|i| Bson::Document(doc! { "i": i as i64 })).collect();
        assert_eq!(find_rows(&docs, "i", FormatOptions::default()).len(), MAX_MATCHES);
    }

    #[test]
    fn table_hits_are_cells_of_top_level_values() {
        let docs = vec![Bson::Document(doc! { "a": { "x": "hit", "y": "hit" }, "b": "hit", "c": "no" }), Bson::Int32(7)];
        let Hits::Cells { list, by_cell } = cell_hits(&docs, "hit", FormatOptions::default()) else { panic!() };
        assert_eq!(list, [(0, "a".to_string()), (0, "b".to_string())], "one hit per cell");
        assert_eq!(by_cell[&(0, "b".to_string())], 1);
        let Hits::Cells { list, .. } = cell_hits(&docs, "7", FormatOptions::default()) else { panic!() };
        assert_eq!(list, [(1, "value".to_string())]);
    }

    #[test]
    fn going_to_a_hit_opens_its_parents_only() {
        let mut p = pane(vec![Bson::Document(doc! { "a": { "b": { "c": "hit" } }, "z": { "y": 1 } })], None);
        p.find.hits = row_hits(p.docs(), "hit", FormatOptions::default());
        p.rows_cache = Some((FormatOptions::default(), "x", Vec::new()));
        p.expanded.clear();
        p.go_to_hit();
        assert_eq!(p.expanded, HashSet::from(["0".to_string(), "0/a".to_string(), "0/a/b".to_string()]));
        assert!(p.rows_cache.is_none(), "the rows change");
        assert_eq!(p.find.scroll, 3);
        p.rows_cache = Some((FormatOptions::default(), "x", Vec::new()));
        p.go_to_hit();
        assert!(p.rows_cache.is_some(), "already open: nothing changes");
    }

    #[test]
    fn find_update_keeps_the_position_unless_the_query_changes() {
        let fmt = FormatOptions::default();
        let docs = [Bson::Document(doc! { "a": "x", "b": "x", "c": "x" })];
        let mut f = PaneFind { query: "x".into(), ..Default::default() };
        assert!(f.update(ViewMode::Tree, fmt, |q| row_hits(&docs, q, fmt)), "new query");
        assert_eq!(f.hits.len(), 3);
        f.current = 2;
        assert!(!f.update(ViewMode::Tree, fmt, |_| panic!("cached")));
        f.stale = true;
        assert!(!f.update(ViewMode::Tree, fmt, |q| row_hits(&docs[..0], q, fmt)), "same query, other page");
        assert_eq!(f.current, 0, "fewer hits");
        f.query = "nothing".into();
        assert!(!f.update(ViewMode::Tree, fmt, |q| row_hits(&docs, q, fmt)), "nothing to show");
    }

    #[test]
    fn copied_text_of_values_and_selections() {
        let fmt = FormatOptions::default();
        let mut p = pane(docs(), None);
        let hover = |p: &mut OutputPane, doc: usize, keys: &[&str], path: &str| {
            p.hovered = Some(HoveredValue { doc, keys: keys.iter().map(|k| k.to_string()).collect(), path: path.into() });
        };
        assert_eq!(p.hovered_text(fmt, false), None, "not on a value");

        // A string as it is, the rest in shell syntax, a document whole.
        hover(&mut p, 0, &["s"], "0/s");
        assert_eq!(p.hovered_text(fmt, false).as_deref(), Some("x"));
        hover(&mut p, 1, &["z"], "1/z");
        assert_eq!(p.hovered_text(fmt, false).as_deref(), Some("true"));
        hover(&mut p, 1, &[], "1");
        assert!(p.hovered_text(fmt, false).unwrap().contains("\"z\" : true"));

        // Tree: the selection when the hovered row is part of it.
        p.expanded.insert("1".into());
        p.rows_cache = Some((fmt, "t", flatten(p.docs(), &p.expanded, fmt, &crate::theme::THEMES[0])));
        p.selected.click("0/s", egui::Modifiers::NONE, Vec::new);
        p.selected.click("1/z", egui::Modifiers::COMMAND, Vec::new);
        hover(&mut p, 1, &["z"], "1/z");
        assert_eq!(p.hovered_text(fmt, false).as_deref(), Some("x\ntrue"), "values, in the order of the rows");
        hover(&mut p, 0, &["_id"], "0/_id");
        assert_eq!(p.hovered_text(fmt, false).as_deref(), Some("1"), "another row: only this one");
        assert_eq!(p.selected_text(fmt, false).as_deref(), Some("x\ntrue"));
        p.selected.click("1", egui::Modifiers::COMMAND, Vec::new);
        assert!(p.selected_text(fmt, false).unwrap().starts_with('['), "with a whole document: the documents");

        // Ctrl+Shift+C: with the names, ready to go inside `{ }`.
        p.selected.clear();
        hover(&mut p, 0, &["s"], "0/s");
        assert_eq!(p.hovered_text(fmt, true).as_deref(), Some("\"s\" : \"x\""));
        hover(&mut p, 1, &[], "1");
        assert_eq!(p.hovered_text(fmt, true), p.hovered_text(fmt, false), "a document has its names");
        assert_eq!(named_value(&["list".into(), "[0]".into()], &Bson::Int32(7), fmt), "\"0\" : 7", "an array element: its index");
        p.selected.click("0/s", egui::Modifiers::NONE, Vec::new);
        p.selected.click("1/z", egui::Modifiers::COMMAND, Vec::new);
        hover(&mut p, 1, &["z"], "1/z");
        assert_eq!(p.hovered_text(fmt, true).as_deref(), Some("\"s\" : \"x\",\n\"z\" : true"));
        assert_eq!(p.selected_text(fmt, true).as_deref(), Some("\"s\" : \"x\",\n\"z\" : true"));
        p.selected.click("1", egui::Modifiers::COMMAND, Vec::new);
        assert_eq!(p.selected_text(fmt, true), p.selected_text(fmt, false), "with a whole document: the documents");

        // Table: documents.
        p.mode = ViewMode::Table;
        p.selected.clear();
        p.selected.click("1", egui::Modifiers::NONE, Vec::new);
        assert!(p.selected_text(fmt, false).unwrap().starts_with('{'));
        p.selected.click("0", egui::Modifiers::COMMAND, Vec::new);
        let both = p.selected_text(fmt, false).unwrap();
        assert!(both.starts_with('[') && both.find("\"s\"") < both.find("\"z\""), "in the order of the page: {both}");
        p.mode = ViewMode::Text;
        assert_eq!(p.selected_text(fmt, false), None);
    }

    #[test]
    fn flatten_respects_expansion() {
        let t = &crate::theme::THEMES[0];
        let d = docs();
        let mut exp = HashSet::new();
        assert_eq!(flatten(&d, &exp, FormatOptions::default(), t).len(), 2);
        exp.insert("0".to_string());
        let rows = flatten(&d, &exp, FormatOptions::default(), t);
        assert_eq!(rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), vec!["(1) 1", "_id", "a", "s", "(2) 2"]);
        expand_all(&mut exp, &d[0], "0".into());
        let rows = flatten(&d, &exp, FormatOptions::default(), t);
        assert!(rows.iter().any(|r| r.path == "0/a/b/[1]/c" && r.depth == 4));
    }

    #[test]
    fn lookup_paths() {
        let d = docs();
        assert_eq!(lookup(&d, 0, &["a".into(), "b".into(), "[1]".into(), "c".into()]), Some(&Bson::Int32(2)));
        assert_eq!(lookup(&d, 1, &["z".into()]), Some(&Bson::Boolean(true)));
        assert_eq!(lookup(&d, 0, &["missing".into()]), None);
        assert_eq!(lookup(&d, 9, &[]), None);
    }

    #[test]
    fn table_columns_union() {
        assert_eq!(table_columns(&docs()), (vec!["_id".to_string(), "a".into(), "s".into(), "z".into()], false));
        assert_eq!(table_columns(&[Bson::Int32(1)]), (vec!["value".to_string()], true));
    }

    #[test]
    fn text_view_is_capped() {
        let big = "x".repeat(600 * 1024);
        let docs: Vec<Bson> = (0..10).map(|i| Bson::Document(doc! { "i": i, "s": big.clone() })).collect();
        let mut p = pane(docs, None);
        p.mode = ViewMode::Text;
        let ctx = egui::Context::default();
        let theme = crate::theme::THEMES[0];
        let mut actions = Vec::new();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let mut cx = PaneCtx { tab: 1, server: 1, index: 0, theme: &theme, fmt: FormatOptions::default(), font_size: 13.0, actions: &mut actions };
            text_view(ui, &mut p, &mut cx);
        });
        // No renderer here: the font atlas upload is discarded on purpose.
        out.textures_delta.clear();
        let (_, text) = p.text_cache.as_ref().unwrap();
        assert!(text.len() < TEXT_VIEW_LIMIT + 700 * 1024);
        assert!(text.contains("more document(s) not shown"));
    }

    #[test]
    fn text_folding() {
        let text = bsonfmt::to_shell_json(&docs()[0], 0, FormatOptions::default()) + "\n";
        let open = fold_text(&text, &HashSet::new());
        assert_eq!(open.text, text);
        // `{`, `"a" : {`, `"b" : [` and `{ "c"` open blocks.
        assert_eq!(open.marks.iter().filter(|m| m.is_some()).count(), 4);
        let a = text.lines().position(|l| l.contains("\"a\" : {")).unwrap();
        let folded = fold_text(&text, &HashSet::from([a]));
        assert!(folded.text.contains("\"a\" : {…},\n"), "{}", folded.text);
        assert!(!folded.text.contains("\"c\""));
        assert_eq!(fold_text(&text, &HashSet::from([0])).text, "{…}\n");
    }

    #[test]
    fn selection_clicks() {
        let order = || ["a", "b", "c", "d"].map(String::from);
        let ctrl = egui::Modifiers::COMMAND;
        let shift = egui::Modifiers::SHIFT;
        let mut s = Selection::default();
        s.click("b", egui::Modifiers::NONE, order);
        s.click("d", ctrl, order);
        assert!(s.contains("b") && s.contains("d") && s.len() == 2);
        s.click("b", ctrl, order);
        assert!(!s.contains("b") && s.len() == 1, "ctrl+click toggles");
        s.click("b", egui::Modifiers::NONE, order);
        s.click("d", shift, order);
        assert_eq!((s.contains("b"), s.contains("c"), s.contains("d"), s.len()), (true, true, true, 3));
        s.click("a", shift, order);
        assert_eq!((s.contains("a"), s.contains("b"), s.len()), (true, true, 2), "range from the anchor, replaced");
        s.click("d", shift | ctrl, order);
        assert_eq!(s.len(), 4, "ctrl+shift adds the range");
        s.context("c");
        assert_eq!(s.len(), 4, "right click inside keeps the selection");
        s.context("z");
        assert!(s.contains("z") && s.len() == 1);
        // Anchor no longer visible: plain click.
        s.click("a", shift, || ["a", "b"].map(String::from));
        assert!(s.contains("a") && s.len() == 1);
    }

    #[test]
    fn paging_state() {
        let spec = QuerySpec { db: "d".into(), coll: "c".into(), filter: doc! {}, projection: None, sort: None, skip: 10, limit: 20, hint: None, collation: None };
        let mut p = pane(docs(), Some(spec));
        assert_eq!((p.skip, p.page_size), (10, 20));
        assert!(p.expanded.contains("0"), "first document auto-expanded");
        assert_eq!(p.source(), Some(("d".to_string(), "c".to_string())));
        p.set_page(vec![Bson::Int32(1)], 30);
        assert_eq!(p.skip, 30);
        assert!(matches!(p.output.kind, OutputKind::Docs { more: false, .. }));
        assert_eq!(p.docs().len(), 1);
        assert!(pane(docs(), None).source().is_none());
    }

    #[test]
    fn table_cells_edited_in_place() {
        let k = |c: &str| vec![c.to_string()];
        let spec = QuerySpec { db: "d".into(), coll: "c".into(), filter: doc! {}, projection: None, sort: None, skip: 0, limit: 0, hint: None, collation: None };
        let date = bson::DateTime::from_millis(1_700_000_000_000);
        let rows = vec![
            Bson::Document(doc! { "_id": 7, "name": "Ada", "n": 5i64, "at": date, "tags": ["x"], "a.b": 1 }),
            Bson::Document(doc! { "name": "no id" }),
        ];
        let mut p = pane(rows.clone(), Some(spec));
        assert_eq!(p.cell_editable(0, &k("_id")), Err("The _id of a document cannot change"));
        assert_eq!(p.cell_editable(0, &k("tags")), Err("Objects and arrays are edited with Edit Document…"));
        assert!(p.cell_editable(0, &k("a.b")).is_err());
        assert_eq!(p.cell_editable(1, &k("name")), Err("The document has no _id"));
        assert!(p.cell_editable(0, &k("missing")).is_ok(), "a field the document lacks can be added");
        assert!(pane(rows.clone(), None).cell_editable(0, &k("name")).is_err(), "not a find");

        // The text reads back as the same value: no change, no command.
        for (column, text) in [("name", "\"Ada\""), ("n", "NumberLong(5)"), ("at", "ISODate(\"2023-11-14T22:13:20.000Z\")")] {
            p.edit_cell(0, &k(column)).unwrap();
            assert_eq!(p.cell_edit.as_ref().unwrap().text, text);
            assert_eq!(p.commit_cell(), None, "{column}");
            assert!(p.cell_edit.is_none());
        }

        // A new value, with its type.
        p.edit_cell(0, &k("n")).unwrap();
        p.cell_edit.as_mut().unwrap().text = "42".into();
        let (db, cmd) = p.commit_cell().unwrap();
        assert_eq!(db, "d");
        assert_eq!(cmd, doc! { "update": "c", "updates": [{ "q": { "_id": 7 }, "u": { "$set": { "n": 42 } }, "upsert": false }] });

        // Bad syntax: the edit stays open with the error.
        p.edit_cell(0, &k("name")).unwrap();
        p.cell_edit.as_mut().unwrap().text = "{ oops".into();
        assert_eq!(p.commit_cell(), None);
        assert!(p.cell_edit.as_ref().unwrap().error.is_some());

        // Empty: nothing to do. Removing a field: $unset.
        p.edit_cell(0, &k("missing")).unwrap();
        assert_eq!(p.cell_edit.as_ref().unwrap().text, "");
        assert_eq!(p.commit_cell(), None);
        let (_, cmd) = p.field_update(0, &k("tags"), None).unwrap();
        assert_eq!(cmd.get_array("updates").unwrap()[0].as_document().unwrap().get_document("u").unwrap(), &doc! { "$unset": { "tags": "" } });

        // A new page ends the edit.
        p.edit_cell(0, &k("name")).unwrap();
        p.set_page(rows, 0);
        assert!(p.cell_edit.is_none());
    }

    #[test]
    fn tree_values_edited_in_place() {
        let spec = QuerySpec { db: "d".into(), coll: "c".into(), filter: doc! {}, projection: None, sort: None, skip: 0, limit: 0, hint: None, collation: None };
        let rows = vec![Bson::Document(doc! { "_id": { "a": 1 }, "address": { "city": "Paris", "zip": { "code": 75 } }, "tags": ["x", { "n": 1 }] })];
        let mut p = pane(rows, Some(spec));
        let keys = |ks: &[&str]| ks.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        assert!(p.cell_editable(0, &keys(&["address", "city"])).is_ok());
        assert!(p.cell_editable(0, &keys(&["tags", "[0]"])).is_ok());
        assert!(p.cell_editable(0, &keys(&["tags", "[1]", "n"])).is_ok());
        assert_eq!(p.cell_editable(0, &keys(&["address", "zip"])), Err(CONTAINER));
        assert_eq!(p.cell_editable(0, &keys(&["_id", "a"])), Err("The _id of a document cannot change"));
        assert_eq!(p.cell_editable(0, &[]), Err("Documents are edited with Edit Document…"));
        assert_eq!(p.cell_editable(0, &keys(&["address", "street"])), Err("Not a value of the document"));

        // Nested fields and array elements, by their dotted path.
        let set = |p: &mut OutputPane, ks: &[&str], text: &str| {
            p.edit_cell(0, &keys(ks)).unwrap();
            p.cell_edit.as_mut().unwrap().text = text.into();
            let (_, cmd) = p.commit_cell().unwrap();
            cmd.get_array("updates").unwrap()[0].as_document().unwrap().get_document("u").unwrap().clone()
        };
        assert_eq!(set(&mut p, &["address", "city"], "\"Lyon\""), doc! { "$set": { "address.city": "Lyon" } });
        assert_eq!(set(&mut p, &["tags", "[0]"], "\"y\""), doc! { "$set": { "tags.0": "y" } });
        assert_eq!(set(&mut p, &["tags", "[1]", "n"], "2"), doc! { "$set": { "tags.1.n": 2 } });
        p.edit_cell(0, &keys(&["address", "city"])).unwrap();
        assert_eq!(p.cell_edit.as_ref().unwrap().text, "\"Paris\"");
        assert_eq!(p.commit_cell(), None, "unchanged");
    }
}
