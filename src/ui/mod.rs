//! Main window: menus, toolbars, docks and the work area.

pub mod connections;
pub mod dialogs;
pub mod editor;
pub mod explorer;
pub mod output;
pub mod query;

use egui::{Align, Color32, Layout, RichText, Ui};

use crate::app::{Action, App, LogLevel};
use crate::settings::{AutocompletionMode, Orientation, TimeZoneMode, UuidEncoding, ViewMode};
use crate::ui::dialogs::Dialog;

pub const ICON_SERVER: &str = "🖥";
pub const ICON_DB: &str = "🗄";
pub const ICON_FOLDER: &str = "📁";
pub const ICON_COLLECTION: &str = "📄";
pub const ICON_VIEW: &str = "👁";
pub const ICON_INDEX: &str = "🔑";
pub const ICON_USER: &str = "👤";
pub const ICON_FUNCTION: &str = "ƒ";
pub const ICON_RUN: &str = "▶";
pub const ICON_STOP: &str = "⏹";
pub const ICON_OPEN: &str = "📂";
pub const ICON_SAVE: &str = "💾";
pub const ICON_CONNECT: &str = "🔌";

/// Small coloured dot used for connection colour tags.
pub fn color_dot(ui: &mut Ui, color: Option<[u8; 3]>, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    match color {
        Some([r, g, b]) => {
            ui.painter().circle_filled(rect.center(), size * 0.4, Color32::from_rgb(r, g, b));
        }
        None => {
            ui.painter().circle_stroke(
                rect.center(),
                size * 0.35,
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
        }
    }
}

fn menu_item(ui: &mut Ui, text: &str, shortcut: &str) -> bool {
    let b = if shortcut.is_empty() {
        egui::Button::new(text)
    } else {
        egui::Button::new(text).shortcut_text(shortcut)
    };
    let clicked = ui.add(b).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn check_item(ui: &mut Ui, value: &mut bool, text: &str) -> bool {
    let changed = ui.checkbox(value, text).changed();
    if changed {
        ui.close();
    }
    changed
}

fn radio_item<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, choice: T, text: &str) -> bool {
    let changed = ui.radio_value(value, choice, text).changed();
    if changed {
        ui.close();
    }
    changed
}

fn menus(app: &mut App, ui: &mut Ui) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            if menu_item(ui, &format!("{ICON_CONNECT} Connect…"), "Ctrl+N") {
                app.actions.push(Action::ShowConnections);
            }
            ui.separator();
            if menu_item(ui, &format!("{ICON_OPEN} Open…"), "Ctrl+O") {
                app.actions.push(Action::OpenFile);
            }
            if menu_item(ui, &format!("{ICON_SAVE} Save"), "Ctrl+S") {
                app.actions.push(Action::SaveFile { save_as: false });
            }
            if menu_item(ui, "Save As…", "Ctrl+Shift+S") {
                app.actions.push(Action::SaveFile { save_as: true });
            }
            ui.separator();
            if menu_item(ui, "Exit", "Ctrl+Q") {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ui.menu_button("View", |ui| {
            if check_item(ui, &mut app.settings.show_explorer, "Explorer  (Ctrl+E)") {
                app.actions.push(Action::SaveSettings);
            }
            if check_item(ui, &mut app.settings.show_logs, "Logs  (Ctrl+L)") {
                app.actions.push(Action::SaveSettings);
            }
            ui.separator();
            ui.menu_button("Theme", |ui| {
                for name in crate::theme::names() {
                    let mut current = app.settings.theme.clone();
                    if ui.radio_value(&mut current, name.to_string(), name).clicked() {
                        app.settings.theme = current;
                        app.actions.push(Action::ApplyTheme);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Font Size", |ui| {
                for size in [11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0] {
                    if radio_item(ui, &mut app.settings.font_size, size, &format!("{size:.0} pt")) {
                        app.actions.push(Action::ApplyTheme);
                    }
                }
            });
            ui.separator();
            let mut o = app.settings.orientation;
            if radio_item(ui, &mut o, Orientation::Vertical, "Editor above results")
                || radio_item(ui, &mut o, Orientation::Horizontal, "Editor beside results  (F10)")
            {
                app.settings.orientation = o;
                app.actions.push(Action::SaveSettings);
            }
            if check_item(ui, &mut app.settings.tabbed_results, "Results in tabs") {
                app.actions.push(Action::SaveSettings);
            }
        });
        ui.menu_button("Options", |ui| {
            ui.menu_button("Default View Mode", |ui| {
                let s = &mut app.settings;
                let mut changed = radio_item(ui, &mut s.view_mode, ViewMode::Tree, "Tree Mode  (F2)");
                changed |= radio_item(ui, &mut s.view_mode, ViewMode::Table, "Table Mode  (F3)");
                changed |= radio_item(ui, &mut s.view_mode, ViewMode::Text, "Text Mode  (F4)");
                if changed {
                    app.actions.push(Action::SaveSettings);
                }
            });
            ui.menu_button("Display Dates In…", |ui| {
                let s = &mut app.settings;
                if radio_item(ui, &mut s.time_zone, TimeZoneMode::Utc, "UTC")
                    | radio_item(ui, &mut s.time_zone, TimeZoneMode::Local, "Local Timezone")
                {
                    app.actions.push(Action::SaveSettings);
                }
            });
            ui.menu_button("Legacy UUID Encoding", |ui| {
                for e in UuidEncoding::ALL {
                    if radio_item(ui, &mut app.settings.uuid_encoding, e, e.label()) {
                        app.actions.push(Action::SaveSettings);
                    }
                }
            });
            ui.menu_button("Autocompletion Mode", |ui| {
                let s = &mut app.settings;
                if radio_item(ui, &mut s.autocompletion, AutocompletionMode::All, "All")
                    | radio_item(ui, &mut s.autocompletion, AutocompletionMode::NoCollectionNames, "No Collection Names")
                    | radio_item(ui, &mut s.autocompletion, AutocompletionMode::None, "None")
                {
                    app.actions.push(Action::SaveSettings);
                }
            });
            ui.separator();
            let s = &mut app.settings;
            let mut changed = check_item(ui, &mut s.load_mongorc, "Load .mongorc.js");
            changed |= check_item(ui, &mut s.auto_expand, "Auto expand first document");
            changed |= check_item(ui, &mut s.line_numbers, "Show line numbers by default");
            changed |= check_item(ui, &mut s.disable_connection_shortcuts, "Disable connection shortcuts");
            changed |= check_item(ui, &mut s.auto_exec, "Automatically execute code in new tab");
            if changed {
                app.actions.push(Action::SaveSettings);
            }
            ui.separator();
            if menu_item(ui, "Change Shell Timeout…", "") {
                app.actions.push(Action::Dialog(Dialog::ShellTimeout { value: app.settings.shell_timeout_secs }));
            }
            if menu_item(ui, "Preferences…", "") {
                app.actions.push(Action::Dialog(Dialog::Preferences { batch_size: app.settings.batch_size }));
            }
        });
        ui.menu_button("Window", |ui| {
            if menu_item(ui, "Full Screen", "F11") {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
            if menu_item(ui, "Minimize", "Ctrl+M") {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            ui.separator();
            if menu_item(ui, "Select Next Tab", "Ctrl+Tab") && !app.tabs.is_empty() {
                app.active_tab = (app.active_tab + 1) % app.tabs.len();
            }
            if menu_item(ui, "Select Previous Tab", "Ctrl+Shift+Tab") && !app.tabs.is_empty() {
                app.active_tab = (app.active_tab + app.tabs.len() - 1) % app.tabs.len();
            }
            ui.separator();
            let tab = app.tabs.get(app.active_tab).map(|t| t.id);
            if menu_item(ui, "Re-execute Query", "Ctrl+R")
                && let Some(t) = tab
            {
                app.actions.push(Action::Execute(t));
            }
            if menu_item(ui, "Duplicate Query In New Tab", "Ctrl+Shift+T")
                && let Some(t) = tab
            {
                app.actions.push(Action::DuplicateTab(t));
            }
        });
        ui.menu_button("Help", |ui| {
            if menu_item(ui, "Keyboard Shortcuts", "") {
                app.actions.push(Action::Dialog(Dialog::Shortcuts));
            }
            if menu_item(ui, "About rustyrobo…", "") {
                app.actions.push(Action::Dialog(Dialog::About));
            }
        });
    });
}

fn toolbar(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let connect = ui.button(format!("{ICON_CONNECT} Connect")).on_hover_text("Manage connections (Ctrl+N)");
        if connect.clicked() {
            app.actions.push(Action::ShowConnections);
        }
        // Quick connect menu, like the Robo 3T drop-down next to Connect.
        ui.menu_button("⏷", |ui| {
            if app.settings.connections.is_empty() {
                ui.label(RichText::new("No connections").weak());
            }
            for (i, c) in app.settings.connections.iter().enumerate() {
                let shortcut = if i < 9 && !app.settings.disable_connection_shortcuts { format!("Alt+{}", i + 1) } else { String::new() };
                ui.horizontal(|ui| {
                    color_dot(ui, c.color, 10.0);
                    if menu_item(ui, &c.name, &shortcut) {
                        app.actions.push(Action::Connect(c.id));
                    }
                });
            }
            ui.separator();
            if menu_item(ui, "Manage Connections…", "") {
                app.actions.push(Action::ShowConnections);
            }
        });
        ui.separator();
        if ui.button(ICON_OPEN).on_hover_text("Open script (Ctrl+O)").clicked() {
            app.actions.push(Action::OpenFile);
        }
        if ui.button(ICON_SAVE).on_hover_text("Save script (Ctrl+S)").clicked() {
            app.actions.push(Action::SaveFile { save_as: false });
        }
        ui.separator();
        let tab = app.tabs.get(app.active_tab);
        let running = tab.is_some_and(|t| t.running);
        let tab_id = tab.map(|t| t.id);
        let exec = egui::Button::new(RichText::new(format!("{ICON_RUN} Execute")).color(app.theme.success));
        if ui.add_enabled(tab_id.is_some() && !running, exec).on_hover_text("Execute (F5 / Ctrl+Enter). Executes the selection if any.").clicked()
            && let Some(t) = tab_id
        {
            app.actions.push(Action::Execute(t));
        }
        let stop = egui::Button::new(RichText::new(ICON_STOP).color(app.theme.error));
        if ui.add_enabled(running, stop).on_hover_text("Stop execution (F6)").clicked()
            && let Some(t) = tab_id
        {
            app.actions.push(Action::Stop(t));
        }
        let orient = match app.settings.orientation {
            Orientation::Vertical => "⬍",
            Orientation::Horizontal => "⬌",
        };
        if ui.button(orient).on_hover_text("Rotate editor/results (F10)").clicked() {
            app.settings.orientation = match app.settings.orientation {
                Orientation::Vertical => Orientation::Horizontal,
                Orientation::Horizontal => Orientation::Vertical,
            };
            app.actions.push(Action::SaveSettings);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some((msg, at, level)) = &app.toast {
                if at.elapsed() < std::time::Duration::from_secs(6) {
                    let c = match level {
                        LogLevel::Error => app.theme.error,
                        LogLevel::Warn => app.theme.warning,
                        LogLevel::Info => app.theme.text_weak,
                    };
                    let short: String = msg.chars().take(120).collect();
                    if ui.add(egui::Label::new(RichText::new(short).color(c)).sense(egui::Sense::click())).on_hover_text("Show logs").clicked() {
                        app.settings.show_logs = true;
                    }
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
                } else {
                    app.toast = None;
                }
            }
        });
    });
}

fn logs_panel(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.strong("Logs");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("✖").on_hover_text("Hide (Ctrl+L)").clicked() {
                app.settings.show_logs = false;
            }
            if ui.small_button("Clear").clicked() {
                app.logs.clear();
            }
        });
    });
    ui.separator();
    let font = egui::FontId::monospace(app.settings.font_size - 1.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        for e in &app.logs {
            let color = match e.level {
                LogLevel::Info => app.theme.text,
                LogLevel::Warn => app.theme.warning,
                LogLevel::Error => app.theme.error,
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(e.at.format("%H:%M:%S").to_string()).font(font.clone()).color(app.theme.text_weak));
                ui.add(egui::Label::new(RichText::new(&e.text).font(font.clone()).color(color)).wrap());
            });
        }
    });
}

pub fn main_window(app: &mut App, ui: &mut Ui) {
    egui::Panel::top("menubar").show(ui, |ui| menus(app, ui));
    egui::Panel::top("toolbar")
        .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(6, 4)))
        .show(ui, |ui| toolbar(app, ui));
    if app.settings.show_logs {
        egui::Panel::bottom("logs")
            .resizable(true)
            .default_size(160.0)
            .min_size(60.0)
            .show(ui, |ui| logs_panel(app, ui));
    }
    if app.settings.show_explorer {
        egui::Panel::left("explorer")
            .resizable(true)
            .default_size(260.0)
            .min_size(160.0)
            .show(ui, |ui| explorer::show(app, ui));
    }
    egui::CentralPanel::default()
        .frame(egui::Frame::central_panel(ui.style()).inner_margin(0))
        .show(ui, |ui| query::work_area(app, ui));
}
