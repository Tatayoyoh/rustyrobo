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

pub struct OutputPane {
    pub output: ShellOutput,
    pub mode: ViewMode,
    pub loading: bool,
    pub skip: u64,
    pub page_size: u64,
    pub expanded: HashSet<String>,
    pub selected: Option<String>,
    /// Derived views, rebuilt only when the page, the expansion state, the
    /// format options or the theme change (not every frame).
    text_cache: Option<(FormatOptions, String)>,
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
            selected: None,
            text_cache: None,
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
        self.rows_cache = None;
        self.columns_cache = None;
        self.selected = None;
    }

    fn source(&self) -> Option<(String, String)> {
        match &self.output.kind {
            OutputKind::Docs { spec: Some(s), .. } => Some((s.db.clone(), s.coll.clone())),
            _ => None,
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
                        (ViewMode::Text, "📝", "Text mode (F4)"),
                        (ViewMode::Table, "☰", "Table mode (F3)"),
                        (ViewMode::Tree, "⊟", "Tree mode (F2)"),
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
                let selected = pane.selected.as_deref() == Some(r.path.as_str());
                row.set_selected(selected);
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
                if resp.clicked() || resp.secondary_clicked() {
                    pane.selected = Some(r.path.clone());
                }
                if resp.double_clicked() && r.container {
                    toggle = Some(r.path.clone());
                }
                resp.context_menu(|ui| {
                    doc_menu(ui, pane, cx_parts(cx), r.doc, Some(&r.keys), Some(&r.path), &mut menu_actions);
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

fn doc_menu(ui: &mut Ui, pane: &OutputPane, cx: MenuCx, doc: usize, keys: Option<&[String]>, path: Option<&str>, out: &mut Vec<MenuAction>) {
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
                    let text = match v {
                        Bson::String(s) => s.clone(),
                        other => bsonfmt::to_shell_json(other, 0, cx.fmt),
                    };
                    ui.ctx().copy_text(text);
                    ui.close();
                }
                if ui.button("Copy Name").clicked() {
                    ui.ctx().copy_text(keys.last().cloned().unwrap_or_default());
                    ui.close();
                }
                if ui.button("Copy Path").clicked() {
                    let p: Vec<String> = keys
                        .iter()
                        .map(|k| k.strip_prefix('[').and_then(|k| k.strip_suffix(']')).unwrap_or(k).to_string())
                        .collect();
                    ui.ctx().copy_text(p.join("."));
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
    if ui.add_enabled(editable, egui::Button::new("Delete Document…")).clicked()
        && let Some(id) = id
    {
        let label = bsonfmt::scalar_shell(&id, cx.fmt);
        out.push(MenuAction::Push(Action::Dialog(Dialog::confirm(
            "Delete Document",
            format!("Delete document with _id {label} from \"{coll}\"?"),
            Action::Command {
                server: cx.server,
                db: db.clone(),
                cmd: doc! { "delete": &coll, "deletes": [{ "q": { "_id": id }, "limit": 1 }] },
                label: format!("Delete document {label}"),
                refresh: Refresh::Tab(cx.tab),
            },
        ))));
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
    let mut select: Option<String> = None;

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
                row.set_selected(pane.selected.as_deref() == Some(path.as_str()));
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
                if resp.clicked() || resp.secondary_clicked() {
                    select = Some(path.clone());
                }
                resp.context_menu(|ui| doc_menu(ui, pane, menu, i, None, None, &mut menu_actions));
            });
        });
    });
    if let Some(s) = select {
        pane.selected = Some(s);
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
    }
    if let Some((_, text)) = &pane.text_cache {
        crate::ui::editor::code_view(ui, Id::new(("out-json", cx.tab, cx.index)), text, cx.theme, FontId::monospace(cx.font_size));
    }
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
        let _ = ctx.run_ui(Default::default(), |ui| {
            let mut cx = PaneCtx { tab: 1, server: 1, index: 0, theme: &theme, fmt: FormatOptions::default(), font_size: 13.0, actions: &mut actions };
            text_view(ui, &mut p, &mut cx);
        });
        let (_, text) = p.text_cache.as_ref().unwrap();
        assert!(text.len() < TEXT_VIEW_LIMIT + 700 * 1024);
        assert!(text.contains("more document(s) not shown"));
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
