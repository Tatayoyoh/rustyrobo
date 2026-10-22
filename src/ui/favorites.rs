//! Favorite scripts manager: scripts filed by connection and database, to
//! add, edit, clone, delete, move by drag and drop, and load into a shell.

use egui::{Align, FontId, Id, Layout, RichText};
use uuid::Uuid;

use crate::app::{Action, App};
use crate::settings::{ConnectionSettings, FavoriteScript};
use crate::ui::editor::{CompletionMode, EditorOptions, code_editor};
use crate::ui::{ICON_DB, ICON_FAVORITE, ICON_SERVER, icon};

#[derive(Default)]
pub struct FavoritesManager {
    pub selected: Option<Uuid>,
    form: Option<Form>,
    confirm_delete: Option<Uuid>,
}

/// Script being added or edited.
struct Form {
    fav: FavoriteScript,
    new: bool,
}

impl FavoritesManager {
    pub fn new(selected: Option<Uuid>) -> Self {
        FavoritesManager { selected, ..Default::default() }
    }
}

/// Favorites of one database.
pub struct DbGroup<'a> {
    pub db: String,
    pub scripts: Vec<&'a FavoriteScript>,
}

/// Favorites of one connection (`None`: any connection).
pub struct ConnGroup<'a> {
    pub connection: Option<Uuid>,
    pub dbs: Vec<DbGroup<'a>>,
}

/// Favorites by connection (in the order of the connection list, unknown
/// ones last), then database (case-insensitive order). Scripts keep the
/// order of the list.
pub fn grouped<'a>(favorites: &'a [FavoriteScript], connections: &[ConnectionSettings]) -> Vec<ConnGroup<'a>> {
    let known = |c: Option<Uuid>| c.filter(|c| connections.iter().any(|k| k.id == *c));
    let mut groups: Vec<ConnGroup<'a>> = Vec::new();
    for f in favorites {
        let conn = known(f.connection);
        let g = match groups.iter_mut().position(|g| g.connection == conn) {
            Some(i) => &mut groups[i],
            None => {
                groups.push(ConnGroup { connection: conn, dbs: Vec::new() });
                groups.last_mut().unwrap()
            }
        };
        match g.dbs.iter_mut().find(|d| d.db == f.db) {
            Some(d) => d.scripts.push(f),
            None => g.dbs.push(DbGroup { db: f.db.clone(), scripts: vec![f] }),
        }
    }
    let rank = |c: Option<Uuid>| c.and_then(|c| connections.iter().position(|k| k.id == c)).unwrap_or(usize::MAX);
    groups.sort_by_key(|g| rank(g.connection));
    for g in &mut groups {
        g.dbs.sort_by_key(|d| d.db.to_lowercase());
    }
    groups
}

/// Moves a favorite to `connection` / `db`, before `before` (a script of
/// that database), or at the end of it.
pub fn move_favorite(favorites: &mut Vec<FavoriteScript>, id: Uuid, connection: Option<Uuid>, db: &str, before: Option<Uuid>) {
    if before == Some(id) {
        return;
    }
    let Some(pos) = favorites.iter().position(|f| f.id == id) else { return };
    let mut f = favorites.remove(pos);
    f.connection = connection;
    f.db = db.to_string();
    let at = before.and_then(|b| favorites.iter().position(|x| x.id == b)).unwrap_or(favorites.len());
    favorites.insert(at, f);
}

/// Where a dragged script was dropped.
struct Drop {
    id: Uuid,
    connection: Option<Uuid>,
    db: Option<String>,
    before: Option<Uuid>,
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut st) = app.favorites.take() else { return };
    let th = app.theme;
    let font = app.settings.font_size;
    let mut actions = Vec::new();
    let mut close = false;

    let order: Vec<Uuid> = grouped(&app.settings.favorites, &app.settings.connections)
        .iter()
        .flat_map(|g| g.dbs.iter().flat_map(|d| d.scripts.iter().map(|f| f.id)))
        .collect();
    if st.selected.is_some_and(|id| !order.contains(&id)) {
        st.selected = None;
    }

    // Keyboard: arrows, Enter loads, Delete asks to delete.
    if st.form.is_none() && st.confirm_delete.is_none() && !ctx.egui_wants_keyboard_input() {
        ctx.input_mut(|i| {
            let pos = st.selected.and_then(|s| order.iter().position(|&o| o == s));
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                && let Some(&n) = order.get(pos.map_or(0, |p| (p + 1).min(order.len() - 1)))
            {
                st.selected = Some(n);
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                && let Some(&n) = order.get(pos.map_or(0, |p| p.saturating_sub(1)))
            {
                st.selected = Some(n);
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                && let Some(id) = st.selected
            {
                actions.push(Action::OpenFavorite(id));
                close = true;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) && st.selected.is_some() {
                st.confirm_delete = st.selected;
            }
        });
    }

    let modal = egui::Modal::new(Id::new("favorites")).show(ctx, |ui| {
        let screen = ui.ctx().content_rect();
        ui.set_width((screen.width() * 0.6).clamp(560.0, 900.0));
        ui.heading("Favorite Scripts");
        ui.separator();

        if let Some(form) = &mut st.form {
            let mut save = false;
            let mut cancel = false;
            egui::Grid::new("fav-form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Name:");
                ui.add(egui::TextEdit::singleline(&mut form.fav.name).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Connection:");
                let label = |c: Option<Uuid>| connection_label(c, &app.settings.connections);
                egui::ComboBox::from_id_salt("fav-conn").selected_text(label(form.fav.connection)).width(260.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut form.fav.connection, None, label(None));
                    for c in &app.settings.connections {
                        ui.selectable_value(&mut form.fav.connection, Some(c.id), &c.name);
                    }
                });
                ui.end_row();
                ui.label("Database:");
                ui.add(egui::TextEdit::singleline(&mut form.fav.db).desired_width(f32::INFINITY));
                ui.end_row();
            });
            ui.add_space(4.0);
            egui::Frame::new().fill(th.base).stroke(egui::Stroke::new(1.0, th.border)).corner_radius(4).inner_margin(4).show(ui, |ui| {
                ui.set_height((screen.height() * 0.4).clamp(160.0, 420.0));
                code_editor(
                    ui,
                    Id::new("fav-editor"),
                    &mut form.fav.script,
                    &th,
                    EditorOptions {
                        line_numbers: true,
                        font: FontId::monospace(font),
                        collections: &[],
                        fields: &std::collections::HashMap::new(),
                        completion: CompletionMode::Off,
                        focus: false,
                        read_only: false,
                        search: true,
                        multi_cursor: true,
                        folding: false,
                    },
                );
            });
            let valid = !form.fav.name.trim().is_empty() && !form.fav.db.trim().is_empty();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if !valid {
                    ui.label(RichText::new("Name and database are required.").color(th.text_weak));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    save = ui.add_enabled(valid, egui::Button::new(RichText::new("Save").strong())).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if save {
                let mut fav = std::mem::take(&mut form.fav);
                fav.name = fav.name.trim().to_string();
                fav.db = fav.db.trim().to_string();
                st.selected = Some(fav.id);
                match app.settings.favorites.iter_mut().find(|f| f.id == fav.id) {
                    Some(f) if !form.new => *f = fav,
                    _ => app.settings.favorites.push(fav),
                }
                actions.push(Action::SaveSettings);
            }
            if save || cancel {
                st.form = None;
            }
            return;
        }

        let sel_index = st.selected.and_then(|id| app.settings.favorites.iter().position(|f| f.id == id));
        ui.horizontal(|ui| {
            if ui.button(format!("{} Add", icon::PLUS)).on_hover_text("New favorite script").clicked() {
                let tab = app.tabs.get(app.active_tab);
                let db = tab.map(|t| t.db.clone()).unwrap_or_default();
                let connection = tab.and_then(|t| app.server(t.server)).map(|s| s.settings.id);
                st.form = Some(Form { fav: FavoriteScript { name: "New script".into(), connection, db, ..Default::default() }, new: true });
            }
            let has_sel = sel_index.is_some();
            if ui.add_enabled(has_sel, egui::Button::new(format!("{} Edit", icon::PENCIL_SIMPLE))).clicked()
                && let Some(i) = sel_index
            {
                st.form = Some(Form { fav: app.settings.favorites[i].clone(), new: false });
            }
            if ui.add_enabled(has_sel, egui::Button::new("Clone")).clicked()
                && let Some(i) = sel_index
            {
                let src = &app.settings.favorites[i];
                let copy = FavoriteScript { id: Uuid::new_v4(), name: format!("Copy of {}", src.name), ..src.clone() };
                st.selected = Some(copy.id);
                app.settings.favorites.insert(i + 1, copy);
                actions.push(Action::SaveSettings);
            }
            if ui.add_enabled(has_sel, egui::Button::new(format!("{} Delete", icon::TRASH))).clicked() {
                st.confirm_delete = st.selected;
            }
        });
        ui.add_space(4.0);

        let mut dropped: Option<Drop> = None;
        let list_h = (screen.height() * 0.45).clamp(200.0, 480.0);
        egui::Frame::new().fill(th.base).stroke(egui::Stroke::new(1.0, th.border)).corner_radius(4).inner_margin(4).show(ui, |ui| {
            ui.set_min_height(list_h);
            if app.settings.favorites.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(list_h * 0.3);
                    ui.label(RichText::new("No favorite scripts yet").strong());
                    ui.label(RichText::new(format!("Click {ICON_FAVORITE} in a shell to add its script, or add one here.")).color(th.text_weak));
                });
                return;
            }
            let drop_line = egui::Stroke::new(2.0, th.accent);
            egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                for g in grouped(&app.settings.favorites, &app.settings.connections) {
                    let conn = g.connection;
                    let header = egui::CollapsingHeader::new(RichText::new(format!("{ICON_SERVER} {}", connection_label(conn, &app.settings.connections))).strong())
                        .id_salt(("fav-conn", conn))
                        .default_open(true)
                        .show(ui, |ui| {
                            for d in &g.dbs {
                                let db_header = egui::CollapsingHeader::new(format!("{ICON_DB} {}", d.db)).id_salt(("fav-db", conn, &d.db)).default_open(true).show(ui, |ui| {
                                    for f in &d.scripts {
                                        let selected = st.selected == Some(f.id);
                                        let preview: String = f.script.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(80).collect();
                                        let drag = ui.dnd_drag_source(Id::new(("fav-drag", f.id)), f.id, |ui| {
                                            ui.add(
                                                egui::Button::selectable(selected, RichText::new(&f.name).strong())
                                                    .right_text(RichText::new(preview).monospace().color(th.text_weak))
                                                    .min_size(egui::vec2(ui.available_width(), 0.0)),
                                            )
                                        });
                                        let r = drag.inner;
                                        // Dropping on a script puts the dragged one before it.
                                        if drag.response.dnd_hover_payload::<Uuid>().is_some_and(|p| *p != f.id) {
                                            let rect = drag.response.rect;
                                            ui.painter().hline(rect.x_range(), rect.top(), drop_line);
                                        }
                                        if let Some(p) = drag.response.dnd_release_payload::<Uuid>() {
                                            dropped = Some(Drop { id: *p, connection: conn, db: Some(d.db.clone()), before: Some(f.id) });
                                        }
                                        if r.clicked() || r.secondary_clicked() {
                                            st.selected = Some(f.id);
                                        }
                                        if r.double_clicked() {
                                            actions.push(Action::OpenFavorite(f.id));
                                            close = true;
                                        }
                                        let id = f.id;
                                        r.context_menu(|ui| {
                                            if ui.button("Load").clicked() {
                                                actions.push(Action::OpenFavorite(id));
                                                close = true;
                                                ui.close();
                                            }
                                            if ui.button("Edit…").clicked() {
                                                st.form = Some(Form { fav: (*f).clone(), new: false });
                                                ui.close();
                                            }
                                            if ui.button("Delete…").clicked() {
                                                st.confirm_delete = Some(id);
                                                ui.close();
                                            }
                                        });
                                    }
                                });
                                // Dropping on a database header moves the script to its end.
                                let hr = &db_header.header_response;
                                if hr.dnd_hover_payload::<Uuid>().is_some() {
                                    ui.painter().rect_stroke(hr.rect, 3, drop_line, egui::StrokeKind::Inside);
                                }
                                if let Some(p) = hr.dnd_release_payload::<Uuid>() {
                                    dropped = Some(Drop { id: *p, connection: conn, db: Some(d.db.clone()), before: None });
                                }
                            }
                        });
                    // Dropping on a connection header keeps the database name.
                    let hr = &header.header_response;
                    if hr.dnd_hover_payload::<Uuid>().is_some() {
                        ui.painter().rect_stroke(hr.rect, 3, drop_line, egui::StrokeKind::Inside);
                    }
                    if let Some(p) = hr.dnd_release_payload::<Uuid>() {
                        dropped = Some(Drop { id: *p, connection: conn, db: None, before: None });
                    }
                }
            });
        });
        if let Some(d) = dropped
            && let Some(f) = app.settings.favorites.iter().find(|f| f.id == d.id)
        {
            let db = d.db.unwrap_or_else(|| f.db.clone());
            move_favorite(&mut app.settings.favorites, d.id, d.connection, &db, d.before);
            st.selected = Some(d.id);
            actions.push(Action::SaveSettings);
        }

        // Deletion confirmation, inline.
        if let Some(del) = st.confirm_delete {
            let name = app.settings.favorites.iter().find(|f| f.id == del).map(|f| f.name.clone()).unwrap_or_default();
            ui.add_space(6.0);
            egui::Frame::new().fill(th.surface).corner_radius(4).inner_margin(8).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Delete favorite script \"{name}\"?")).color(th.warning));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Delete").color(th.error)).clicked() {
                            app.settings.favorites.retain(|f| f.id != del);
                            for t in app.tabs.iter_mut().filter(|t| t.favorite == Some(del)) {
                                t.favorite = None;
                            }
                            st.confirm_delete = None;
                            actions.push(Action::SaveSettings);
                        }
                        if ui.button("Keep").clicked() {
                            st.confirm_delete = None;
                        }
                    });
                });
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Double-click or Enter to load · drag to move · Del to delete").small().color(th.text_weak));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(st.selected.is_some(), egui::Button::new(RichText::new("Load").strong())).clicked()
                    && let Some(id) = st.selected
                {
                    actions.push(Action::OpenFavorite(id));
                    close = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    });
    // Escape closes the form first, then the manager.
    if modal.should_close() {
        if st.form.is_some() || st.confirm_delete.is_some() {
            st.form = None;
            st.confirm_delete = None;
        } else {
            close = true;
        }
    }
    if !close {
        app.favorites = Some(st);
    }
    app.actions.extend(actions);
}

fn connection_label(c: Option<Uuid>, connections: &[ConnectionSettings]) -> String {
    c.and_then(|c| connections.iter().find(|k| k.id == c)).map_or_else(|| "Any connection".to_string(), |c| c.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fav(name: &str, conn: Option<Uuid>, db: &str) -> FavoriteScript {
        FavoriteScript { name: name.into(), connection: conn, db: db.into(), ..Default::default() }
    }

    #[test]
    fn groups_by_connection_then_database() {
        let a = ConnectionSettings { name: "A".into(), ..Default::default() };
        let b = ConnectionSettings { name: "B".into(), ..Default::default() };
        let conns = vec![a.clone(), b.clone()];
        let gone = Some(Uuid::new_v4());
        let favs = vec![fav("1", Some(b.id), "shop"), fav("2", None, "x"), fav("3", Some(a.id), "shop"), fav("4", Some(b.id), "Admin"), fav("5", Some(b.id), "shop"), fav("6", gone, "x")];
        let g = grouped(&favs, &conns);
        type Shape<'a> = Vec<(Option<Uuid>, Vec<(&'a str, Vec<&'a str>)>)>;
        let shape: Shape<'_> = g
            .iter()
            .map(|g| (g.connection, g.dbs.iter().map(|d| (d.db.as_str(), d.scripts.iter().map(|f| f.name.as_str()).collect())).collect()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (Some(a.id), vec![("shop", vec!["3"])]),
                (Some(b.id), vec![("Admin", vec!["4"]), ("shop", vec!["1", "5"])]),
                (None, vec![("x", vec!["2", "6"])]),
            ],
            "connection order, then databases; deleted connections with no connection"
        );
    }

    #[test]
    fn moves_between_groups() {
        let c = Some(Uuid::new_v4());
        let mut favs = vec![fav("a", None, "x"), fav("b", None, "x"), fav("c", c, "y")];
        let (a, b, cc) = (favs[0].id, favs[1].id, favs[2].id);
        move_favorite(&mut favs, b, None, "x", Some(a));
        assert_eq!(favs.iter().map(|f| f.id).collect::<Vec<_>>(), vec![b, a, cc], "reordered");
        move_favorite(&mut favs, a, c, "y", None);
        assert_eq!((favs[2].id, favs[2].connection, favs[2].db.as_str()), (a, c, "y"), "moved to the end of another database");
        move_favorite(&mut favs, cc, c, "y", Some(cc));
        assert_eq!(favs[1].id, cc, "dropped on itself: unchanged");
    }
}
