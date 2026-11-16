//! Scripted UI walkthrough used to check the interface without a human:
//! `RUSTYROBO_AUTOTEST=<dir>` drives the app through its main screens and
//! saves a PNG screenshot of each into `<dir>`, then quits.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::collapsing_header::CollapsingState;
use egui::{Context, Id, Rect, pos2, vec2};

use crate::app::{Action, App, Zone};
use crate::settings::ViewMode;
use crate::theme::THEMES;
use crate::ui::connections::{ConnectionEditor, Section};
use crate::ui::dialogs::{Dialog, DocMode};

pub struct AutoTest {
    dir: PathBuf,
    next_at: Instant,
    step: usize,
    /// The screenshot asked for: its name, and the part of the window kept.
    pending_shot: Option<(String, Option<Rect>)>,
}

/// Step of the first theme of the query screenshots, which end the walkthrough.
const THEMES_STEP: usize = 36;

impl AutoTest {
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("RUSTYROBO_AUTOTEST")?);
        let _ = std::fs::create_dir_all(&dir);
        Some(AutoTest { dir, next_at: Instant::now() + Duration::from_millis(1500), step: 0, pending_shot: None })
    }
}

fn open(ctx: &Context, id: Id) {
    let mut s = CollapsingState::load_with_default_open(ctx, id, false);
    s.set_open(true);
    s.store(ctx);
}

/// A screenshot of the window, or of the part of it in `crop`.
fn shot(t: &mut AutoTest, ctx: &Context, name: &str, crop: Option<Rect>) {
    t.pending_shot = Some((name.to_string(), crop.map(|r| r.intersect(ctx.content_rect()))));
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
}

/// Where the dialog on top was drawn in the last frame, with room for its
/// shadow: its widgets but the backdrop, which covers the window.
fn dialog(ctx: &Context) -> Option<Rect> {
    let screen = ctx.content_rect();
    let layer = ctx.memory(|m| m.areas().top_layer_id(egui::Order::Foreground))?;
    let rect = ctx.viewport(|v| v.prev_pass.widgets.get_layer(layer).map(|w| w.rect).filter(|r| !r.contains_rect(screen)).fold(Rect::NOTHING, Rect::union));
    rect.is_positive().then(|| rect.expand(12.0))
}

fn zone(app: &App, zone: Zone) -> Option<Rect> {
    app.zones.iter().find(|(z, _)| *z == zone).map(|(_, r)| *r)
}

/// The top of the results: their first rows.
fn results(app: &App) -> Option<Rect> {
    zone(app, Zone::Results).map(|r| Rect::from_min_size(r.min, vec2(r.width(), 440.0)))
}

/// The query, from its tab down to the first rows of its results.
fn query(app: &App) -> Option<Rect> {
    let results = zone(app, Zone::Results)?;
    let explorer = zone(app, Zone::Explorer).unwrap_or(results);
    Some(Rect::from_min_max(pos2(explorer.max.x.min(results.min.x), explorer.min.y), pos2(results.max.x, results.min.y + 400.0)))
}

pub fn tick(app: &mut App, ctx: &Context) {
    let Some(mut t) = app.autotest.take() else { return };

    // Save screenshots delivered this frame.
    let images: Vec<_> = ctx.input(|i| {
        i.raw
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
            .collect()
    });
    for img in images {
        if let Some((name, crop)) = t.pending_shot.take() {
            let path = t.dir.join(format!("{name}.png"));
            let img = match crop {
                Some(r) => cropped(&img, r, ctx.pixels_per_point()),
                None => (*img).clone(),
            };
            if let Err(e) = save_png(&path, &img) {
                eprintln!("autotest: {e}");
            } else {
                eprintln!("autotest: saved {}", path.display());
            }
        }
    }

    ctx.request_repaint_after(Duration::from_millis(100));
    if Instant::now() < t.next_at || t.pending_shot.is_some() {
        app.autotest = Some(t);
        return;
    }
    let mut wait = 1.0;
    let conn_ssh = app.settings.connections.get(2).cloned();
    let conn_main = app.settings.connections.first().map(|c| c.id);
    match t.step {
        0 => app.conn_list = None,
        1 => shot(&mut t, ctx, "00-welcome", None),
        2 => app.conn_list = Some(crate::ui::connections::ConnectionList::new(&app.settings)),
        3 => shot(&mut t, ctx, "01-connections", dialog(ctx)),
        4 => {
            if let Some(c) = conn_ssh.clone() {
                app.conn_editor = Some(ConnectionEditor::new(c, false));
            }
        }
        5 => shot(&mut t, ctx, "02-editor-general", dialog(ctx)),
        6 => {
            if let Some(e) = &mut app.conn_editor {
                e.section = Section::Ssh;
            }
        }
        7 => shot(&mut t, ctx, "03-editor-ssh", dialog(ctx)),
        8 => {
            if let Some(e) = &mut app.conn_editor {
                e.run_test = true;
            }
            wait = 3.0;
        }
        9 => shot(&mut t, ctx, "04-editor-test", dialog(ctx)),
        10 => {
            app.conn_editor = None;
            app.conn_list = None;
            if let Some(id) = conn_main {
                app.actions.push(Action::Connect(id));
            }
            wait = 2.5;
        }
        11 => {
            if let Some(s) = app.servers.first() {
                let sid = s.id;
                open(ctx, Id::new(("srv", sid)));
                open(ctx, Id::new(("db", sid, "rrtest")));
                open(ctx, Id::new(("db", sid, "rrtest")).with("collections"));
                open(ctx, Id::new(("coll", sid, "rrtest", "items")));
                open(ctx, Id::new(("coll", sid, "rrtest", "items")).with("indexes"));
                app.actions.push(Action::OpenShell {
                    server: sid,
                    db: "rrtest".into(),
                    script: "db.getCollection('items').find({})".into(),
                    execute: true,
                    title: "items".into(),
                    cursor: None,
                });
            }
            wait = 2.5;
        }
        12 => shot(&mut t, ctx, "05-tree", None),
        13 => set_mode(app, ViewMode::Table),
        14 => shot(&mut t, ctx, "06-table", results(app)),
        15 => set_mode(app, ViewMode::Text),
        16 => shot(&mut t, ctx, "07-text", results(app)),
        17 => {
            if let Some(tab) = app.tabs.last_mut() {
                tab.script = "db.items.countDocuments({ tag: \"fizz\" })\nprint(\"hello from the shell\")\ndb.items.aggregate([{ $group: { _id: \"$tag\", n: { $sum: 1 } } }])\ndb.items.find({ $bad: 1 })".into();
                let id = tab.id;
                app.actions.push(Action::Execute(id));
            }
            wait = 2.0;
        }
        18 => shot(&mut t, ctx, "08-multi", None),
        19 => {
            app.settings.theme = "Porcelain".into();
            app.actions.push(Action::ApplyTheme);
            if let Some(s) = app.servers.first() {
                app.dialog = Some(Dialog::document(
                    DocMode::Edit,
                    s.id,
                    "rrtest",
                    "items",
                    "{\n    \"_id\" : ObjectId(\"5f1d7a0e1c9d440000a1b2c3\"),\n    \"n\" : 1,\n    \"at\" : ISODate(\"2024-01-01T00:00:00.000Z\")\n}".into(),
                    None,
                    None,
                ));
            }
        }
        20 => shot(&mut t, ctx, "10-light-doc", dialog(ctx)),
        21 => {
            app.settings.theme = crate::theme::DEFAULT_THEME.into();
            app.actions.push(Action::ApplyTheme);
            let release = std::sync::Arc::new(crate::update::Release {
                version: "9.9.9".into(),
                notes: "### Added\n- A new feature\n- Another one\n\n### Fixed\n- A bug".into(),
                asset: Some("rustyrobo-v9.9.9.tar.gz".into()),
            });
            app.update = Some(std::sync::Arc::clone(&release));
            app.dialog = None;
        }
        22 => shot(&mut t, ctx, "11-update-chip", None),
        23 => app.dialog = Some(Dialog::Update { release: app.update.clone().expect("set above"), os: crate::update::Os::current(), commands_open: true }),
        24 => shot(&mut t, ctx, "12-update", dialog(ctx)),
        25 => {
            app.dialog = None;
            app.update = None;
            // The scripts executed above are in the history.
            app.history_window = Some(Default::default());
        }
        26 => shot(&mut t, ctx, "13-history", dialog(ctx)),
        27 => {
            app.history_window = None;
            if let Some(s) = app.servers.first() {
                app.dialog = Some(Dialog::export(s.id, "rrtest", "items"));
            }
        }
        28 => shot(&mut t, ctx, "14-export", dialog(ctx)),
        29 => {
            app.dialog = None;
            if let Some(tab) = app.tabs.last_mut() {
                tab.script = "db.getCollection('items').find({})".into();
                let id = tab.id;
                app.actions.push(Action::Execute(id));
            }
            wait = 2.0;
        }
        30 => {
            set_mode(app, ViewMode::Table);
            if let Some(pane) = app.tabs.last_mut().and_then(|t| t.outputs.first_mut()) {
                let _ = pane.edit_cell(1, &["tag".to_string()]);
                if let Some(e) = &mut pane.cell_edit {
                    e.text = "\"fizzbuzz\"".into();
                }
            }
        }
        31 => shot(&mut t, ctx, "15-table-edit", results(app)),
        32 => {
            if let Some(tab) = app.tabs.last_mut().and_then(|t| t.outputs.first_mut()) {
                tab.cell_edit = None;
            }
            if let Some(s) = app.servers.first() {
                let listed = match s.dbs.iter().find(|d| d.name == "rrtest").map(|d| &d.collections) {
                    Some(crate::app::Load::Ready(v)) => v.iter().map(|c| c.name.clone()).collect(),
                    _ => Vec::new(),
                };
                app.dialog = Some(Dialog::dump(s.id, "rrtest", listed));
            }
        }
        33 => shot(&mut t, ctx, "16-dump", dialog(ctx)),
        // The themes: a query on nested documents, the first one opened.
        34 => {
            app.dialog = None;
            if let Some(s) = app.servers.first() {
                app.actions.push(Action::OpenShell {
                    server: s.id,
                    db: "shop".into(),
                    script: "// Paid orders, the biggest first\ndb.getCollection('orders').find({\n    status: 'paid',\n    total: { $gt: 20 }\n}).sort({ total: -1 })".into(),
                    execute: true,
                    title: "orders".into(),
                    cursor: None,
                });
            }
            wait = 2.5;
        }
        35 => {
            if let Some(pane) = app.tabs.last_mut().and_then(|t| t.outputs.first_mut()) {
                pane.expand(&["0", "0/customer", "0/items", "0/items/[0]"]);
            }
        }
        n if (THEMES_STEP..THEMES_STEP + 2 * THEMES.len()).contains(&n) => {
            let theme = THEMES[(n - THEMES_STEP) / 2].name;
            if (n - THEMES_STEP).is_multiple_of(2) {
                app.settings.theme = theme.into();
                app.actions.push(Action::ApplyTheme);
            } else {
                shot(&mut t, ctx, &format!("09-theme-{}", theme.to_lowercase().replace(' ', "-")), query(app));
            }
        }
        _ => {
            app.dialog = None;
            app.history_window = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    t.step += 1;
    t.next_at = Instant::now() + Duration::from_secs_f32(wait);
    app.autotest = Some(t);
}

fn set_mode(app: &mut App, mode: ViewMode) {
    if let Some(tab) = app.tabs.last_mut() {
        for o in &mut tab.outputs {
            o.mode = mode;
        }
    }
}

/// The part `r` (in points) of a screenshot.
fn cropped(img: &egui::ColorImage, r: Rect, ppp: f32) -> egui::ColorImage {
    let [w, h] = img.size;
    let (x, y) = (((r.min.x * ppp) as usize).min(w), ((r.min.y * ppp) as usize).min(h));
    img.region_by_pixels([x, y], [((r.width() * ppp) as usize).min(w - x), ((r.height() * ppp) as usize).min(h - y)])
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
    w.write_image_data(&bytes)?;
    Ok(())
}
