//! Work area: query tabs (shell editor + result panes).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange};
use egui::{Align, FontId, Id, Layout, RichText, Ui};

pub use crate::ui::output::OutputPane;

use crate::app::{Action, App, ServerStatus};
use crate::settings::{AutocompletionMode, Orientation};
use crate::shell::ShellHandle;
use crate::ui::editor::{CompletionMode, EditorOptions, code_editor};
use crate::ui::output::{PaneCtx, show_pane};
use crate::ui::{ICON_DB, ICON_SERVER, color_dot};

pub struct QueryTab {
    pub id: u64,
    pub server: u64,
    pub db: String,
    pub title: String,
    pub script: String,
    pub shell: Option<ShellHandle>,
    pub running: bool,
    pub pending: Option<u64>,
    pub started: Option<Instant>,
    pub elapsed: Option<Duration>,
    pub outputs: Vec<OutputPane>,
    pub notice: Option<String>,
    pub file: Option<PathBuf>,
    pub dirty: bool,
    pub selection: Option<String>,
    pub cursor_hint: Option<usize>,
    pub result_tab: usize,
    pub line_numbers: bool,
    focus_editor: bool,
}

impl QueryTab {
    pub fn new(id: u64, server: u64, db: String, title: String, script: String, shell: ShellHandle) -> Self {
        QueryTab {
            id,
            server,
            db,
            title,
            script,
            shell: Some(shell),
            running: false,
            pending: None,
            started: None,
            elapsed: None,
            outputs: Vec::new(),
            notice: None,
            file: None,
            dirty: false,
            selection: None,
            cursor_hint: None,
            result_tab: 0,
            line_numbers: false,
            focus_editor: true,
        }
    }
}

pub fn work_area(app: &mut App, ui: &mut Ui) {
    if app.tabs.is_empty() {
        welcome(app, ui);
        return;
    }
    app.active_tab = app.active_tab.min(app.tabs.len() - 1);
    tab_bar(app, ui);
    let idx = app.active_tab;
    let mut actions = Vec::new();
    let (tab_server, tab_db) = (app.tabs[idx].server, app.tabs[idx].db.clone());
    let collections: &[String] = app
        .servers
        .iter()
        .find(|s| s.id == tab_server)
        .and_then(|s| s.dbs.iter().find(|d| d.name == tab_db))
        .map(|d| d.coll_names.as_slice())
        .unwrap_or(&[]);
    let server_info = app.tabs.get(idx).and_then(|t| app.server(t.server)).map(|s| {
        (s.settings.name.clone(), s.settings.address_label(), s.settings.color, matches!(s.status, ServerStatus::Connected(_)))
    });
    // Borrowed (not cloned) every frame: `app.fields` and `app.tabs` are disjoint.
    let empty_fields = std::collections::HashMap::new();
    let fields = app.fields.get(&(tab_server, tab_db)).unwrap_or(&empty_fields);
    let theme = app.theme;
    let fmt = app.fmt();
    let font_size = app.settings.font_size;
    let orientation = app.settings.orientation;
    let tabbed = app.settings.tabbed_results;
    let completion = match app.settings.autocompletion {
        AutocompletionMode::All => CompletionMode::All,
        AutocompletionMode::NoCollectionNames => CompletionMode::NoCollectionNames,
        AutocompletionMode::None => CompletionMode::Off,
    };
    let tab = &mut app.tabs[idx];

    // Info bar: server and database, like the Robo 3T query header.
    egui::Frame::new().fill(theme.bg).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            if let Some((name, addr, color, connected)) = &server_info {
                color_dot(ui, *color, 10.0);
                let c = if *connected { theme.text } else { theme.error };
                ui.label(RichText::new(format!("{ICON_SERVER} {name}")).strong().color(c));
                ui.label(RichText::new(addr).color(theme.text_weak));
                ui.separator();
            }
            ui.label(RichText::new(format!("{ICON_DB} {}", tab.db)).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.selectable_label(tab.line_numbers, "#").on_hover_text("Line numbers").clicked() {
                    tab.line_numbers = !tab.line_numbers;
                }
                if tab.running {
                    let secs = tab.started.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
                    if ui.button(RichText::new("⏹ Stop").color(theme.error)).clicked() {
                        actions.push(Action::Stop(tab.id));
                    }
                    ui.label(RichText::new(format!("{secs:.1} s")).color(theme.text_weak));
                    ui.spinner();
                } else if let Some(e) = tab.elapsed {
                    ui.label(RichText::new(format!("{:.3} sec.", e.as_secs_f64())).color(theme.text_weak));
                }
            });
        });
    });

    let editor_id = Id::new(("editor", tab.id));
    if let Some(c) = tab.cursor_hint.take() {
        let tid = editor_id.with("text");
        let mut st = egui::TextEdit::load_state(ui.ctx(), tid).unwrap_or_default();
        st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(c))));
        egui::TextEdit::store_state(ui.ctx(), tid, st);
        tab.focus_editor = true;
    }

    let line_numbers = tab.line_numbers;
    let mut want_fields = None;
    let mut editor = |ui: &mut Ui, tab: &mut QueryTab| {
        egui::Frame::new().fill(theme.base).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
            let r = code_editor(
                ui,
                editor_id,
                &mut tab.script,
                &theme,
                EditorOptions {
                    line_numbers,
                    font: FontId::monospace(font_size),
                    collections,
                    fields,
                    completion,
                    focus: std::mem::take(&mut tab.focus_editor),
                    read_only: false,
                },
            );
            if r.changed {
                tab.dirty = true;
            }
            if let Some(coll) = r.want_fields {
                want_fields = Some((tab.server, tab.db.clone(), coll));
            }
            if r.has_focus || r.selection.is_some() {
                tab.selection = r.selection;
            }
        });
    };

    let panel_id = Id::new(("editor-panel", tab.id));
    match orientation {
        Orientation::Vertical => {
            egui::Panel::top(panel_id)
                .resizable(true)
                .default_size(170.0)
                .min_size(60.0)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| editor(ui, tab));
        }
        Orientation::Horizontal => {
            egui::Panel::left(panel_id)
                .resizable(true)
                .default_size(420.0)
                .min_size(160.0)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| editor(ui, tab));
        }
    }

    if let Some((server, db, coll)) = want_fields {
        actions.push(Action::LoadFields(server, db, coll));
    }
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(4)))
        .show(ui, |ui| {
            if let Some(n) = &tab.notice {
                ui.label(RichText::new(n).color(theme.text_weak));
            }
            if tab.outputs.is_empty() {
                if tab.running {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Executing…");
                    });
                }
                return;
            }
            let mut cx = PaneCtx { tab: tab.id, server: tab.server, index: 0, theme: &theme, fmt, font_size, actions: &mut actions };
            let n = tab.outputs.len();
            if tabbed && n > 1 {
                ui.horizontal_wrapped(|ui| {
                    for (i, o) in tab.outputs.iter().enumerate() {
                        let label = o.output.statement.lines().next().unwrap_or("").chars().take(32).collect::<String>();
                        if ui.selectable_label(tab.result_tab == i, format!("{} {label}", i + 1)).clicked() {
                            tab.result_tab = i;
                        }
                    }
                });
                ui.separator();
                let i = tab.result_tab.min(n - 1);
                cx.index = i;
                show_pane(ui, &mut tab.outputs[i], &mut cx);
            } else if n == 1 {
                show_pane(ui, &mut tab.outputs[0], &mut cx);
            } else {
                // Stack panes vertically, each with an equal share (min 160 px).
                let avail = ui.available_height();
                let each = ((avail - 6.0 * n as f32) / n as f32).max(160.0);
                egui::ScrollArea::vertical().id_salt(("outs", tab.id)).auto_shrink([false, false]).show(ui, |ui| {
                    for (i, pane) in tab.outputs.iter_mut().enumerate() {
                        cx.index = i;
                        let w = ui.available_width();
                        ui.allocate_ui(egui::vec2(w, each), |ui| {
                            ui.set_min_height(each);
                            show_pane(ui, pane, &mut cx);
                        });
                        ui.add_space(6.0);
                    }
                });
            }
        });
    app.actions.extend(actions);
}

fn tab_bar(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let mut close = None;
    let mut actions = Vec::new();
    egui::Frame::new().fill(theme.surface).inner_margin(egui::Margin::symmetric(4, 2)).show(ui, |ui| {
        egui::ScrollArea::horizontal().id_salt("tabbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, t) in app.tabs.iter().enumerate() {
                    let active = i == app.active_tab;
                    let color = app.server(t.server).and_then(|s| s.settings.color);
                    let fill = if active { theme.base } else { theme.surface };
                    egui::Frame::new()
                        .fill(fill)
                        .corner_radius(egui::CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 })
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .stroke(if active { egui::Stroke::new(1.0, theme.border) } else { egui::Stroke::NONE })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if color.is_some() {
                                    color_dot(ui, color, 8.0);
                                }
                                let mut title = t.title.clone();
                                if t.dirty && t.file.is_some() {
                                    title.push_str(" •");
                                }
                                let text = if active { RichText::new(title).strong() } else { RichText::new(title).color(theme.text_weak) };
                                let r = ui.add(egui::Label::new(text).sense(egui::Sense::click()).selectable(false));
                                if t.running {
                                    ui.spinner();
                                }
                                let r = r.on_hover_text(format!("{}\n{}", t.db, t.script.lines().next().unwrap_or("")));
                                if r.clicked() {
                                    app.active_tab = i;
                                }
                                if r.middle_clicked() {
                                    close = Some(t.id);
                                }
                                r.context_menu(|ui| {
                                    if ui.button("New Shell").clicked() {
                                        actions.push(Action::OpenShell { server: t.server, db: t.db.clone(), script: String::new(), execute: false, title: "New Shell".into(), cursor: None });
                                        ui.close();
                                    }
                                    if ui.button("Re-execute Query").clicked() {
                                        actions.push(Action::Execute(t.id));
                                        ui.close();
                                    }
                                    if ui.button("Duplicate Query In New Tab").clicked() {
                                        actions.push(Action::DuplicateTab(t.id));
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.button("Close Shell").clicked() {
                                        actions.push(Action::CloseTab(t.id));
                                        ui.close();
                                    }
                                    if ui.button("Close Other Shells").clicked() {
                                        for o in app.tabs.iter().filter(|o| o.id != t.id) {
                                            actions.push(Action::CloseTab(o.id));
                                        }
                                        ui.close();
                                    }
                                    if ui.button("Close Shells to the Right").clicked() {
                                        for o in app.tabs.iter().skip(i + 1) {
                                            actions.push(Action::CloseTab(o.id));
                                        }
                                        ui.close();
                                    }
                                });
                                if ui.add(egui::Button::new(RichText::new("✖").small().color(theme.text_weak)).frame(false)).on_hover_text("Close (Ctrl+W)").clicked() {
                                    close = Some(t.id);
                                }
                            });
                        });
                }
            });
        });
    });
    if let Some(id) = close {
        actions.push(Action::CloseTab(id));
    }
    app.actions.extend(actions);
}

fn welcome(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.12);
        if let Some(logo) = &app.logo {
            ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(160.0, 160.0)));
        }
        ui.label(RichText::new("Rustyrobo").size(34.0).strong().color(theme.accent));
        ui.label(RichText::new("Native MongoDB management tool").color(theme.text_weak));
        ui.add_space(18.0);
        if ui.button(RichText::new(format!("{} Manage connections…", crate::ui::ICON_CONNECT)).size(16.0)).clicked() {
            app.actions.push(Action::ShowConnections);
        }
        ui.add_space(14.0);
        let mut recent: Vec<_> = app.settings.connections.iter().filter(|c| c.last_connected.is_some()).collect();
        recent.sort_by_key(|c| std::cmp::Reverse(c.last_connected));
        if !recent.is_empty() {
            ui.label(RichText::new("Recent connections").strong());
            ui.add_space(4.0);
            for c in recent.into_iter().take(6) {
                ui.horizontal(|ui| {
                    let w = 280.0;
                    ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                    color_dot(ui, c.color, 10.0);
                    if ui.link(&c.name).clicked() {
                        app.actions.push(Action::Connect(c.id));
                    }
                    ui.label(RichText::new(c.address_label()).color(theme.text_weak));
                });
            }
        }
        ui.add_space(20.0);
        ui.label(RichText::new("F5 / Ctrl+Enter execute · F6 stop · F2/F3/F4 tree/table/text · Ctrl+T new shell").small().color(theme.text_weak));
    });
}
