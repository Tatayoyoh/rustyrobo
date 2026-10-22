//! Result panes: tree / table / text views of documents, paging and the
//! document context menu (view, edit, insert, delete, copy).

use std::collections::HashSet;

use bson::{Bson, doc};
use egui::{Align, FontId, Id, Layout, RichText, Ui};
use egui_extras::{Column, TableBuilder};

use crate::app::{Action, Refresh};
use crate::bsonfmt::{self, FormatOptions};
use crate::settings::ViewMode;
use crate::shell::{OutputKind, ShellOutput};
use crate::theme::{Theme, bson_color};
use crate::ui::dialogs::{Dialog, DocMode};
use crate::ui::editing::{FoldedText, fold_text};
use crate::ui::editor::FoldClick;

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
        }
    }

    pub fn docs(&self) -> &[Bson] {
        match &self.output.kind {
            OutputKind::Docs { docs, .. } => docs,
            _ => &[],
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

pub fn show_pane(ui: &mut Ui, pane: &mut OutputPane, cx: &mut PaneCtx<'_>) {
    header(ui, pane, cx);
    ui.add_space(2.0);
    match &pane.output.kind {
        OutputKind::Text(t) => {
            let t = t.clone();
            crate::ui::editor::code_view(ui, Id::new(("out-text", cx.tab, cx.index)), &t, cx.theme, FontId::monospace(cx.font_size));
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
                    OutputKind::Docs { .. } => ("{}", "Result".to_string()),
                    OutputKind::Text(_) => ("📝", "Text".to_string()),
                    OutputKind::Error(_) => ("⚠", "Error".to_string()),
                };
                let color = if matches!(pane.output.kind, OutputKind::Error(_)) { th.error } else { th.text };
                ui.label(RichText::new(format!("{icon} {title}")).strong().color(color))
                    .on_hover_text(&pane.output.statement);
                ui.label(RichText::new(format!("{:.3} sec.", pane.output.elapsed.as_secs_f64())).color(th.text_weak));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !matches!(pane.output.kind, OutputKind::Docs { .. }) {
                        return;
                    }
                    for (mode, label, tip) in [
                        (ViewMode::Text, "📝 Text", "Text mode (F4)"),
                        (ViewMode::Table, "☰ Table", "Table mode (F3)"),
                        (ViewMode::Tree, "⊟ Tree", "Tree mode (F2)"),
                    ] {
                        if ui.selectable_label(pane.mode == mode, label).on_hover_text(tip).clicked() {
                            pane.mode = mode;
                        }
                    }
                    ui.separator();
                    paging(ui, pane, cx);
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
    let next = ui.add_enabled(more && !pane.loading, egui::Button::new("▶").small()).on_hover_text("Next page");
    let size = ui.add(egui::TextEdit::singleline(&mut pane.size_input).desired_width(36.0)).on_hover_text("Page size (limit)");
    let skip = ui.add(egui::TextEdit::singleline(&mut pane.skip_input).desired_width(46.0)).on_hover_text("Skip");
    let prev = ui.add_enabled(pane.skip > 0 && !pane.loading, egui::Button::new("◀").small()).on_hover_text("Previous page");
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

    TableBuilder::new(ui)
        .id_salt(("tree", cx.tab, cx.index))
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::initial(280.0).at_least(80.0).clip(true))
        .column(Column::initial(320.0).at_least(80.0).clip(true))
        .column(Column::remainder().at_least(70.0).clip(true))
        .auto_shrink([false, false])
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
                row.col(|ui| {
                    ui.add_space(indent * r.depth as f32);
                    if r.container {
                        let open = pane.expanded.contains(&r.path);
                        let arrow = if open { "⏷" } else { "⏵" };
                        if ui.add(egui::Button::new(RichText::new(arrow).color(th.text_weak)).frame(false).small()).clicked() {
                            toggle = Some(r.path.clone());
                        }
                    } else {
                        ui.add_space(indent);
                    }
                    let key_color = if r.depth == 0 { th.text } else { th.syntax.property };
                    ui.add(egui::Label::new(RichText::new(&r.key).font(font.clone()).color(key_color)).selectable(false).truncate());
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new(&r.value).font(mono.clone()).color(r.color)).selectable(false).truncate());
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new(r.tname).color(th.text_weak)).selectable(false).truncate());
                });
                let resp = row.response();
                if resp.clicked() {
                    let mods = resp.ctx.input(|i| i.modifiers);
                    pane.selected.click(&r.path, mods, || rows.iter().map(|r| r.path.clone()));
                }
                if resp.secondary_clicked() {
                    pane.selected.context(&r.path);
                }
                if resp.double_clicked() && r.container {
                    toggle = Some(r.path.clone());
                }
                resp.context_menu(|ui| {
                    let targets: Vec<Target<'_>> = if pane.selected.len() > 1 && pane.selected.contains(&r.path) {
                        rows.iter().filter(|r| pane.selected.contains(&r.path)).map(|r| Target { doc: r.doc, keys: Some(&r.keys), path: Some(&r.path) }).collect()
                    } else {
                        vec![Target { doc: r.doc, keys: Some(&r.keys), path: Some(&r.path) }]
                    };
                    doc_menu(ui, pane, cx_parts(cx), &targets, &mut menu_actions);
                });
            });
        });

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
    server: u64,
    fmt: FormatOptions,
}

fn cx_parts(cx: &PaneCtx<'_>) -> MenuCx {
    MenuCx { tab: cx.tab, server: cx.server, fmt: cx.fmt }
}

#[allow(clippy::large_enum_variant)] // built only on menu clicks
enum MenuAction {
    Push(Action),
    ExpandAll(String),
    CollapseAll(String),
}

fn apply_menu_actions(pane: &mut OutputPane, cx: &mut PaneCtx<'_>, list: Vec<MenuAction>) {
    for a in list {
        match a {
            MenuAction::Push(a) => cx.actions.push(a),
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
        if ui.button("Expand Recursively").clicked() {
            out.extend(containers.iter().map(|p| MenuAction::ExpandAll(p.to_string())));
            ui.close();
        }
        if ui.button("Collapse Recursively").clicked() {
            out.extend(containers.iter().map(|p| MenuAction::CollapseAll(p.to_string())));
            ui.close();
        }
        ui.separator();
    }
    // Values inside documents (not the documents themselves): one per line.
    if values.iter().all(|(t, _)| t.keys.is_some_and(|k| !k.is_empty())) {
        if ui.button("Copy Values").clicked() {
            ui.ctx().copy_text(values.iter().map(|(_, v)| copy_value(v, cx.fmt)).collect::<Vec<_>>().join("\n"));
            ui.close();
        }
        if ui.button("Copy Names").clicked() {
            ui.ctx().copy_text(values.iter().filter_map(|(t, _)| t.keys?.last().cloned()).collect::<Vec<_>>().join("\n"));
            ui.close();
        }
        if ui.button("Copy Paths").clicked() {
            ui.ctx().copy_text(values.iter().filter_map(|(t, _)| t.keys.map(dotted_path)).collect::<Vec<_>>().join("\n"));
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
    if ui.button(format!("Copy JSON ({n} document{plural})")).clicked() {
        ui.ctx().copy_text(bsonfmt::to_shell_json(&Bson::Array(roots.iter().map(|&b| b.clone()).collect()), 0, cx.fmt));
        ui.close();
    }
    if ui.button(format!("Copy Extended JSON ({n} document{plural})")).clicked() {
        let v: Vec<serde_json::Value> = roots.iter().map(|&b| b.clone().into_relaxed_extjson()).collect();
        ui.ctx().copy_text(serde_json::to_string_pretty(&v).unwrap_or_default());
        ui.close();
    }
    // Deleting needs whole documents selected, not values inside them.
    let whole = targets.iter().all(|t| t.keys.is_none_or(|k| k.is_empty()));
    if whole && let Some((n, action)) = delete_documents(pane, cx.server, cx.tab, &indices, cx.fmt) {
        ui.separator();
        let plural = if n == 1 { "" } else { "s" };
        if ui.add(egui::Button::new(format!("Delete {n} Document{plural}…")).shortcut_text("Del")).clicked() {
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

/// `a.b.0.c` from tree keys (`[0]` for array elements).
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
            if ui.button("Expand Recursively").clicked() {
                out.push(MenuAction::ExpandAll(path.to_string()));
                ui.close();
            }
            if ui.button("Collapse Recursively").clicked() {
                out.push(MenuAction::CollapseAll(path.to_string()));
                ui.close();
            }
            ui.separator();
        }
        if !keys.is_empty()
            && let Some(v) = value {
                if ui.button("Copy Value").clicked() {
                    ui.ctx().copy_text(copy_value(v, cx.fmt));
                    ui.close();
                }
                if ui.button("Copy Name").clicked() {
                    ui.ctx().copy_text(keys.last().cloned().unwrap_or_default());
                    ui.close();
                }
                if ui.button("Copy Path").clicked() {
                    ui.ctx().copy_text(dotted_path(keys));
                    ui.close();
                }
                ui.separator();
            }
    }

    if ui.button("Copy JSON").clicked() {
        ui.ctx().copy_text(bsonfmt::to_shell_json(root, 0, cx.fmt));
        ui.close();
    }
    if ui.button("Copy Extended JSON (relaxed)").clicked() {
        let v = root.clone().into_relaxed_extjson();
        ui.ctx().copy_text(serde_json::to_string_pretty(&v).unwrap_or_default());
        ui.close();
    }
    ui.separator();
    let text = bsonfmt::to_shell_json(root, 0, cx.fmt);
    let (db, coll) = source.clone().unwrap_or_default();
    if ui.button("View Document…").clicked() {
        out.push(MenuAction::Push(Action::Dialog(Dialog::document(DocMode::View, cx.server, &db, &coll, text.clone(), None, None))));
        ui.close();
    }
    let editable = source.is_some() && id.is_some();
    if ui.add_enabled(editable, egui::Button::new("Edit Document…")).clicked() {
        out.push(MenuAction::Push(Action::Dialog(Dialog::document(DocMode::Edit, cx.server, &db, &coll, text.clone(), id.clone(), Some(cx.tab)))));
        ui.close();
    }
    if ui.add_enabled(source.is_some(), egui::Button::new("Insert Document…")).clicked() {
        out.push(MenuAction::Push(Action::Dialog(Dialog::document(DocMode::Insert, cx.server, &db, &coll, "{\n    \n}".into(), None, Some(cx.tab)))));
        ui.close();
    }
    let delete = delete_documents(pane, cx.server, cx.tab, &[doc], cx.fmt);
    if ui.add_enabled(delete.is_some(), egui::Button::new("Delete Document…").shortcut_text("Del")).clicked()
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
    let docs = pane.docs();
    let row_h = cx.font_size + 8.0;
    let th = cx.theme;
    let mono = FontId::monospace(cx.font_size);
    let fmt = cx.fmt;
    let menu = cx_parts(cx);
    let mut menu_actions = Vec::new();
    let mut click: Option<(String, Option<egui::Modifiers>)> = None;

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
        tb.header(row_h, |mut h| {
            h.col(|ui| {
                ui.strong("#");
            });
            for c in &columns {
                h.col(|ui| {
                    ui.strong(c);
                });
            }
        })
        .body(|body| {
            body.rows(row_h, docs.len(), |mut row| {
                let i = row.index();
                let d = &docs[i];
                let path = i.to_string();
                row.set_selected(pane.selected.contains(&path));
                row.col(|ui| {
                    ui.label(RichText::new((pane.skip + i as u64 + 1).to_string()).color(th.text_weak));
                });
                for c in &columns {
                    row.col(|ui| {
                        let v = if scalar_mode { Some(d) } else { d.as_document().and_then(|doc| doc.get(c)) };
                        if let Some(v) = v {
                            ui.add(
                                egui::Label::new(RichText::new(bsonfmt::short_value(v, fmt)).font(mono.clone()).color(bson_color(th, v)))
                                    .selectable(false)
                                    .truncate(),
                            );
                        }
                    });
                }
                let resp = row.response();
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
                    doc_menu(ui, pane, menu, &targets, &mut menu_actions);
                });
            });
        });
    });
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
    let click = crate::ui::editor::folding_code_view(ui, Id::new(("out-json", cx.tab, cx.index)), &view.text, &view.marks, cx.theme, FontId::monospace(cx.font_size));
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
}
