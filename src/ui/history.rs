//! Query history window: the scripts executed in the shells, newest first,
//! to search, look at, and open again in a new shell.

use egui::{Align, FontId, Id, Layout, RichText};
use uuid::Uuid;

use crate::app::{Action, App};
use crate::history::HistoryEntry;
use crate::settings::ConnectionSettings;
use crate::ui::editor::code_view;
use crate::ui::{ICON_HISTORY, icon};

#[derive(Default)]
pub struct HistoryWindow {
    pub query: String,
    pub selected: Option<Uuid>,
    confirm_clear: bool,
    /// Entries matching the query, newest first, with what they were
    /// computed for: the query, the number of entries and the newest one.
    found: Option<(String, usize, Option<Uuid>, Vec<usize>)>,
    focused: bool,
}

impl HistoryWindow {
    fn found(&mut self, app: &App) -> &[usize] {
        let entries = &app.history.entries;
        let key = (self.query.clone(), entries.len(), entries.last().map(|e| e.id));
        let stale = self.found.as_ref().is_none_or(|(q, n, last, _)| (q, n, last) != (&key.0, &key.1, &key.2));
        if stale {
            let list = app.history.search(&self.query);
            self.found = Some((key.0, key.1, key.2, list));
        }
        self.found.as_ref().map(|f| f.3.as_slice()).unwrap_or_default()
    }
}

/// Name of the connection of an entry: its current name, else the one it had.
pub fn connection_label(e: &HistoryEntry, connections: &[ConnectionSettings]) -> String {
    match e.connection.and_then(|c| connections.iter().find(|k| k.id == c)) {
        Some(c) => c.name.clone(),
        None if !e.connection_name.is_empty() => e.connection_name.clone(),
        None => "No connection".into(),
    }
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut st) = app.history_window.take() else { return };
    let th = app.theme;
    let font = app.settings.font_size;
    let mut actions = Vec::new();
    let mut close = false;

    let found: Vec<usize> = st.found(app).to_vec();
    let ids: Vec<Uuid> = found.iter().map(|&i| app.history.entries[i].id).collect();
    if st.selected.is_none_or(|s| !ids.contains(&s)) {
        st.selected = ids.first().copied();
    }

    // Arrows and Enter work from the search field; Del outside of it.
    if !st.confirm_clear {
        ctx.input_mut(|i| {
            let pos = st.selected.and_then(|s| ids.iter().position(|&o| o == s));
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                && let Some(&n) = ids.get(pos.map_or(0, |p| (p + 1).min(ids.len().saturating_sub(1))))
            {
                st.selected = Some(n);
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                && let Some(&n) = ids.get(pos.map_or(0, |p| p.saturating_sub(1)))
            {
                st.selected = Some(n);
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                && let Some(id) = st.selected
            {
                actions.push(Action::OpenHistory(id));
                close = true;
            }
        });
        if !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
            && let Some(id) = st.selected
        {
            actions.push(Action::DeleteHistory(id));
        }
    }

    let modal = egui::Modal::new(Id::new("history")).show(ctx, |ui| {
        let screen = ui.ctx().content_rect();
        ui.set_width((screen.width() * 0.6).clamp(560.0, 900.0));
        ui.heading(format!("{ICON_HISTORY} Query History"));
        ui.separator();

        let search = ui.add(
            egui::TextEdit::singleline(&mut st.query)
                .hint_text(format!("{} Search scripts, databases and connections", icon::MAGNIFYING_GLASS))
                .desired_width(f32::INFINITY),
        );
        if !st.focused {
            search.request_focus();
            st.focused = true;
        }
        ui.add_space(4.0);

        let list_h = (screen.height() * 0.3).clamp(160.0, 360.0);
        egui::Frame::new().fill(th.base).stroke(egui::Stroke::new(1.0, th.border)).corner_radius(4).inner_margin(4).show(ui, |ui| {
            ui.set_min_height(list_h);
            if found.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(list_h * 0.3);
                    if app.history.entries.is_empty() {
                        ui.label(RichText::new("No queries yet").strong());
                        ui.label(RichText::new("The scripts you execute are listed here.").color(th.text_weak));
                    } else {
                        ui.label(RichText::new("No matching queries").color(th.text_weak));
                    }
                });
                return;
            }
            let row_h = ui.spacing().interact_size.y;
            let open_label = format!("{} Open", icon::ARROW_SQUARE_OUT);
            let open_w = egui::WidgetText::from(open_label.as_str()).into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button).size().x
                + 2.0 * ui.spacing().button_padding.x;
            egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show_rows(ui, row_h, found.len(), |ui, rows| {
                for &i in &found[rows] {
                    let e = &app.history.entries[i];
                    let selected = st.selected == Some(e.id);
                    let line: String = e.first_line().chars().take(100).collect();
                    let at = e.at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M");
                    let meta = format!("{} · {} · {at}", connection_label(e, &app.settings.connections), e.db);
                    // Room on the right for the open and delete buttons, shown
                    // on hover, far apart to avoid deleting by mistake.
                    let open_id = Id::new(("history-open", e.id));
                    let del_id = Id::new(("history-delete", e.id));
                    let row = egui::Button::selectable(selected, RichText::new(line).monospace())
                        .right_text((
                            RichText::new(meta).color(th.text_weak),
                            egui::Atom::custom(open_id, egui::vec2(open_w, row_h - 4.0)),
                            egui::Atom::custom(Id::new("history-gap"), egui::vec2(24.0, 0.0)),
                            egui::Atom::custom(del_id, egui::Vec2::splat(row_h - 4.0)),
                        ))
                        .min_size(egui::vec2(ui.available_width(), row_h))
                        .atom_ui(ui);
                    if ui.rect_contains_pointer(row.response.rect) {
                        if let Some(rect) = row.rect(open_id)
                            && ui.put(rect, egui::Button::new(open_label.as_str())).on_hover_text("Open in a new shell (Enter)").clicked()
                        {
                            actions.push(Action::OpenHistory(e.id));
                            close = true;
                        }
                        if let Some(rect) = row.rect(del_id)
                            && ui.put(rect, egui::Button::new(icon::TRASH).frame_when_inactive(false)).on_hover_text("Delete from the history (Del)").clicked()
                        {
                            actions.push(Action::DeleteHistory(e.id));
                        }
                    }
                    let r = row.response;
                    if selected && ui.input(|i| i.key_down(egui::Key::ArrowDown) || i.key_down(egui::Key::ArrowUp)) {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() || r.secondary_clicked() {
                        st.selected = Some(e.id);
                    }
                    if r.double_clicked() {
                        actions.push(Action::OpenHistory(e.id));
                        close = true;
                    }
                    let id = e.id;
                    r.context_menu(|ui| {
                        if ui.button("Open in New Shell").clicked() {
                            actions.push(Action::OpenHistory(id));
                            close = true;
                            ui.close();
                        }
                        if ui.button("Copy Script").clicked() {
                            ui.ctx().copy_text(app.history.get(id).map(|e| e.script.clone()).unwrap_or_default());
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            actions.push(Action::DeleteHistory(id));
                            ui.close();
                        }
                    });
                }
            });
        });

        // The script of the selected entry.
        ui.add_space(4.0);
        let preview_h = (screen.height() * 0.22).clamp(100.0, 260.0);
        egui::Frame::new().fill(th.base).stroke(egui::Stroke::new(1.0, th.border)).corner_radius(4).inner_margin(4).show(ui, |ui| {
            ui.set_height(preview_h);
            let script = st.selected.and_then(|id| app.history.get(id)).map(|e| e.script.as_str()).unwrap_or_default();
            code_view(ui, Id::new("history-preview"), script, &th, FontId::monospace(font));
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut app.settings.query_history, "Record executed scripts").changed() {
                actions.push(Action::SaveSettings);
            }
            ui.label(RichText::new("· double-click or Enter to open · Del to delete").small().color(th.text_weak));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let sel = st.selected;
                if ui.add_enabled(sel.is_some(), egui::Button::new(RichText::new("Open").strong())).on_hover_text("Open in a new shell").clicked()
                    && let Some(id) = sel
                {
                    actions.push(Action::OpenHistory(id));
                    close = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
                if ui.add_enabled(sel.is_some(), egui::Button::new(icon::COPY)).on_hover_text("Copy the script").clicked()
                    && let Some(e) = sel.and_then(|id| app.history.get(id))
                {
                    ui.ctx().copy_text(e.script.clone());
                }
                if ui.add_enabled(!app.history.entries.is_empty(), egui::Button::new("Clear All…")).clicked() {
                    st.confirm_clear = true;
                }
            });
        });
    });
    // Clear All asks in a popup over the window.
    if st.confirm_clear {
        let confirm = egui::Modal::new(Id::new("history-clear")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Clear History");
            ui.add_space(4.0);
            ui.label(format!("Clear the {} queries of the history?", app.history.entries.len()));
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(RichText::new("Clear").color(th.error)).clicked() {
                    app.history.entries.clear();
                    st.confirm_clear = false;
                    actions.push(Action::SaveHistory);
                }
                if ui.button("Keep").clicked() {
                    st.confirm_clear = false;
                }
            });
        });
        if confirm.should_close() {
            st.confirm_clear = false;
        }
    }
    // A window that settles on its size a frame later would stay misplaced
    // until the next input (see `dialogs::show`).
    let rect_id = Id::new("history-rect");
    if ctx.data(|d| d.get_temp::<egui::Rect>(rect_id)) != Some(modal.response.rect) {
        ctx.data_mut(|d| d.insert_temp(rect_id, modal.response.rect));
        ctx.request_repaint();
    }
    if modal.should_close() && !st.confirm_clear {
        close = true;
    }
    if !close {
        app.history_window = Some(st);
    }
    app.actions.extend(actions);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_names() {
        let c = ConnectionSettings { name: "Renamed".into(), ..Default::default() };
        let e = HistoryEntry { connection: Some(c.id), connection_name: "Old".into(), ..Default::default() };
        assert_eq!(connection_label(&e, std::slice::from_ref(&c)), "Renamed");
        assert_eq!(connection_label(&e, &[]), "Old", "deleted connection");
        assert_eq!(connection_label(&HistoryEntry::default(), &[]), "No connection");
    }
}
