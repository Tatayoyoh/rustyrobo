//! Ctrl+P: opens a collection by its name, from a bar at the top of the window
//! that lists the collections of the connected servers matching what is typed.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, FontId, Id, Key, Modifiers};

use crate::app::{Action, App, Load};
use crate::ui::{ICON_COLLECTION, ICON_VIEW, icon};

/// Rows shown at once; the others scroll.
const ROWS: usize = 12;

#[derive(Default)]
pub struct QuickOpen {
    query: String,
    selected: usize,
    focused: bool,
    /// Matches for `key`, best first.
    items: Vec<Item>,
    key: Option<(String, usize)>,
}

#[derive(Clone, Debug, PartialEq)]
struct Item {
    server: u64,
    db: String,
    coll: String,
    view: bool,
}

/// Opens the bar, or closes it. The collections of the databases not expanded
/// yet are loaded, to be found too.
pub fn toggle(app: &mut App) {
    if app.quick_open.take().is_some() {
        return;
    }
    for s in app.servers.iter().filter(|s| s.conn().is_some()) {
        for d in s.dbs.iter().filter(|d| matches!(d.collections, Load::Idle)) {
            app.actions.push(Action::LoadCollections(s.id, d.name.clone()));
        }
    }
    app.quick_open = Some(QuickOpen::default());
}

/// Rank of a collection for `query` (lower first), `None` when it does not
/// match: the start of its name, inside it, inside `db.name`, then the letters
/// of the query in this order. Case is ignored.
fn rank(query: &str, db: &str, coll: &str) -> Option<u8> {
    let (query, coll) = (query.to_lowercase(), coll.to_lowercase());
    let full = format!("{}.{coll}", db.to_lowercase());
    let mut letters = full.chars();
    if coll.starts_with(&query) {
        Some(0)
    } else if coll.contains(&query) {
        Some(1)
    } else if full.contains(&query) {
        Some(2)
    } else if query.chars().filter(|c| !c.is_whitespace()).all(|q| letters.any(|c| c == q)) {
        Some(3)
    } else {
        None
    }
}

fn matches(app: &App, query: &str) -> Vec<Item> {
    let mut found: Vec<(u8, Item)> = Vec::new();
    for s in &app.servers {
        for d in &s.dbs {
            for c in d.collections.ready().into_iter().flatten() {
                if let Some(r) = rank(query.trim(), &d.name, &c.name) {
                    found.push((r, Item { server: s.id, db: d.name.clone(), coll: c.name.clone(), view: c.kind == "view" }));
                }
            }
        }
    }
    // Stable: servers, databases and collections stay in the order of the
    // explorer, when nothing is typed and between equal matches.
    if !query.trim().is_empty() {
        found.sort_by_key(|(r, item)| (*r, item.coll.len()));
    }
    found.into_iter().map(|(_, item)| item).collect()
}

/// Collections loaded: the matches are computed again when it changes.
fn loaded(app: &App) -> usize {
    app.servers.iter().flat_map(|s| &s.dbs).map(|d| d.collections.ready().map_or(0, |c| c.len() + 1)).sum()
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut q) = app.quick_open.take() else { return };
    let key = (q.query.clone(), loaded(app));
    if q.key.as_ref() != Some(&key) {
        if q.key.as_ref().is_some_and(|k| k.0 != key.0) {
            q.selected = 0;
        }
        q.items = matches(app, &q.query);
        q.key = Some(key);
    }
    q.selected = q.selected.min(q.items.len().saturating_sub(1));
    let loading = app.servers.iter().flat_map(|s| &s.dbs).any(|d| matches!(d.collections, Load::Loading));
    let several = app.servers.iter().filter(|s| s.dbs.iter().any(|d| d.collections.ready().is_some())).count() > 1;
    let names: std::collections::HashMap<u64, &str> = app.servers.iter().map(|s| (s.id, s.settings.name.as_str())).collect();
    let theme = app.theme;
    let font = FontId::proportional(app.settings.font_size);
    let mut open = None;
    let mut close = false;

    let (down, up) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp)));
    if down && q.selected + 1 < q.items.len() {
        q.selected += 1;
    }
    if up {
        q.selected = q.selected.saturating_sub(1);
    }
    // The click that opened it, from a menu, is not one elsewhere.
    let opening = !q.focused;
    let width = (ctx.content_rect().width() - 32.0).min(560.0);
    let area = egui::Area::new(Id::new("quick-open")).order(egui::Order::Foreground).anchor(Align2::CENTER_TOP, egui::vec2(0.0, 56.0)).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(width);
            let r = ui.add(egui::TextEdit::singleline(&mut q.query).id(Id::new("quick-open-query")).hint_text(format!("{} Open collection…", icon::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
            if !q.focused {
                r.request_focus();
                q.focused = true;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                open = q.items.get(q.selected).cloned();
                close = true;
            }
            if q.items.is_empty() {
                let text = if loading { "Loading collections…" } else { "No collection matches" };
                ui.label(egui::RichText::new(text).color(theme.text_weak));
                return;
            }
            let row_h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical().max_height(row_h * ROWS as f32).auto_shrink([false, true]).show_rows(ui, row_h, q.items.len(), |ui, rows| {
                // Full-width rows, their text on the left, as in a menu.
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                    for i in rows {
                        let item = &q.items[i];
                        let mut job = LayoutJob::default();
                        let glyph = if item.view { ICON_VIEW } else { ICON_COLLECTION };
                        job.append(&format!("{glyph} {}", item.coll), 0.0, TextFormat::simple(font.clone(), theme.text));
                        let place = match names.get(&item.server).filter(|_| several) {
                            Some(server) => format!("{} · {server}", item.db),
                            None => item.db.clone(),
                        };
                        job.append(&place, 12.0, TextFormat::simple(FontId::proportional(app.settings.font_size * 0.9), theme.text_weak));
                        let r = ui.add(egui::Button::selectable(i == q.selected, job).min_size(egui::vec2(0.0, row_h)));
                        if i == q.selected && (up || down) {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            open = Some(item.clone());
                            close = true;
                        }
                    }
                });
            });
        });
    });
    if area.response.clicked_elsewhere() && !opening {
        close = true;
    }
    if let Some(item) = open {
        app.actions.push(crate::ui::explorer::view_documents(item.server, &item.db, &item.coll));
    }
    if !close && app.quick_open.is_none() {
        app.quick_open = Some(q);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_the_start_of_a_name_first_then_inside_then_letters_in_order() {
        assert_eq!(rank("ord", "shop", "orders"), Some(0));
        assert_eq!(rank("ORD", "shop", "Orders"), Some(0), "case is ignored");
        assert_eq!(rank("der", "shop", "orders"), Some(1));
        assert_eq!(rank("shop.or", "shop", "orders"), Some(2));
        assert_eq!(rank("sord", "shop", "orders"), Some(3));
        assert_eq!(rank("sh ord", "shop", "orders"), Some(3), "spaces are ignored");
        assert_eq!(rank("dros", "shop", "orders"), None, "letters in another order");
        assert_eq!(rank("", "shop", "orders"), Some(0));
    }
}
