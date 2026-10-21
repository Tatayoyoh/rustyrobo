//! Scripted UI walkthrough used to check the interface without a human:
//! `RUSTYROBO_AUTOTEST=<dir>` drives the app through its main screens and
//! saves a PNG screenshot of each into `<dir>`, then quits.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::collapsing_header::CollapsingState;
use egui::{Context, Id};

use crate::app::{Action, App};
use crate::settings::ViewMode;
use crate::ui::connections::{ConnectionEditor, Section};
use crate::ui::dialogs::{Dialog, DocMode};

pub struct AutoTest {
    dir: PathBuf,
    next_at: Instant,
    step: usize,
    pending_shot: Option<String>,
}

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

fn shot(t: &mut AutoTest, ctx: &Context, name: &str) {
    t.pending_shot = Some(name.to_string());
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
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
        if let Some(name) = t.pending_shot.take() {
            let path = t.dir.join(format!("{name}.png"));
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
        1 => shot(&mut t, ctx, "00-welcome"),
        2 => app.conn_list = Some(crate::ui::connections::ConnectionList::new(&app.settings)),
        3 => shot(&mut t, ctx, "01-connections"),
        4 => {
            if let Some(c) = conn_ssh.clone() {
                app.conn_editor = Some(ConnectionEditor::new(c, false));
            }
        }
        5 => shot(&mut t, ctx, "02-editor-general"),
        6 => {
            if let Some(e) = &mut app.conn_editor {
                e.section = Section::Ssh;
            }
        }
        7 => shot(&mut t, ctx, "03-editor-ssh"),
        8 => {
            if let Some(e) = &mut app.conn_editor {
                e.run_test = true;
            }
            wait = 3.0;
        }
        9 => shot(&mut t, ctx, "04-editor-test"),
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
        12 => shot(&mut t, ctx, "05-tree"),
        13 => set_mode(app, ViewMode::Table),
        14 => shot(&mut t, ctx, "06-table"),
        15 => set_mode(app, ViewMode::Text),
        16 => shot(&mut t, ctx, "07-text"),
        17 => {
            if let Some(tab) = app.tabs.last_mut() {
                tab.script = "db.items.countDocuments({ tag: \"fizz\" })\nprint(\"hello from the shell\")\ndb.items.aggregate([{ $group: { _id: \"$tag\", n: { $sum: 1 } } }])\ndb.items.find({ $bad: 1 })".into();
                let id = tab.id;
                app.actions.push(Action::Execute(id));
            }
            wait = 2.0;
        }
        18 => shot(&mut t, ctx, "08-multi"),
        19 => {
            app.settings.theme = "Nord".into();
            app.actions.push(Action::ApplyTheme);
        }
        20 => shot(&mut t, ctx, "09-theme-nord"),
        21 => {
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
        22 => shot(&mut t, ctx, "10-light-doc"),
        _ => {
            app.dialog = None;
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
