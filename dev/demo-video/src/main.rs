//! Records the frames of the demo video (see `dev/demo-video.sh`): Rustyrobo
//! at 960x720 in its test harness, driven by the keyboard, the keys shown at
//! the bottom. Usage: `rustyrobo-demo-video <frames dir> <port of the demo server>`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Color32, Key, Modifiers, RichText};
use egui_kittest::Harness;
use rustyrobo::app::{App, Load};
use rustyrobo::settings::{AppSettings, ConnectionSettings, HostPort};
use rustyrobo::ui::icon;

const FPS: f32 = 30.0;
/// How long the keys stay on screen, the last part fading out (seconds).
const KEYS_SHOWN: f64 = 2.4;
const KEYS_FADE: f64 = 0.3;

/// The app, with the keys last pressed over it.
struct Demo {
    app: App,
    keys: Vec<String>,
    /// egui time of the last keys.
    since: f64,
}

impl eframe::App for Demo {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
        let age = ui.input(|i| i.time) - self.since;
        if self.keys.is_empty() || age > KEYS_SHOWN {
            return;
        }
        let opacity = ((KEYS_SHOWN - age) / KEYS_FADE).min(1.0) as f32;
        egui::Area::new(egui::Id::new("demo-keys"))
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -28.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                ui.multiply_opacity(opacity);
                // Light keys on a dark plate with a shadow: they stand out on any theme.
                egui::Frame::new()
                    .fill(Color32::from_gray(18))
                    .stroke(egui::Stroke::new(2.0, Color32::from_gray(200)))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::same(10))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 24, spread: 4, color: Color32::from_black_alpha(200) })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (i, key) in self.keys.iter().enumerate() {
                                if i > 0 {
                                    ui.label(RichText::new("+").size(28.0).strong().color(Color32::WHITE));
                                }
                                egui::Frame::new()
                                    .fill(Color32::from_gray(245))
                                    .stroke(egui::Stroke::new(1.0, Color32::from_gray(150)))
                                    .corner_radius(7)
                                    .inner_margin(egui::Margin::symmetric(16, 6))
                                    .show(ui, |ui| ui.label(RichText::new(key).size(28.0).strong().color(Color32::from_gray(15))));
                            }
                        });
                    });
            });
    }

    fn on_exit(&mut self) {
        self.app.on_exit();
    }
}

struct Recorder {
    h: Harness<'static, Demo>,
    dir: PathBuf,
    frames: usize,
}

impl Recorder {
    fn frame(&mut self) {
        self.h.step();
        let image = self.h.render().expect("render");
        image.save(self.dir.join(format!("{:05}.png", self.frames))).expect("save the frame");
        self.frames += 1;
    }

    fn hold(&mut self, secs: f32) {
        for _ in 0..(secs * FPS) as usize {
            self.frame();
        }
    }

    fn show_keys(&mut self, keys: &[&str]) {
        let now = self.h.ctx.input(|i| i.time);
        let demo = self.h.state_mut();
        demo.keys = keys.iter().map(|k| k.to_string()).collect();
        demo.since = now;
    }

    fn press(&mut self, modifiers: Modifiers, key: Key, keys: &[&str]) {
        self.show_keys(keys);
        self.h.key_press_modifiers(modifiers, key);
        self.frame();
    }

    fn type_text(&mut self, text: &str) {
        for (i, c) in text.char_indices() {
            self.show_keys(&[&text[..i + c.len_utf8()]]);
            self.h.event(egui::Event::Text(c.to_string()));
            self.hold(0.12);
        }
    }

    /// Where `word` is drawn, after `before`, in a text of the last frame.
    fn text_pos(&self, before: &str, word: &str) -> egui::Pos2 {
        for c in &self.h.output().shapes {
            if let egui::Shape::Text(t) = &c.shape
                && let Some(at) = t.galley.job.text.find(&format!("{before}{word}"))
            {
                let start = t.galley.job.text[..at + before.len()].chars().count();
                let a = t.galley.pos_from_cursor(egui::text::CCursor::new(start));
                let b = t.galley.pos_from_cursor(egui::text::CCursor::new(start + word.chars().count()));
                return t.pos + (a.center().to_vec2() + b.center().to_vec2()) / 2.0;
            }
        }
        panic!("{word:?} is not shown");
    }

    /// The pointer goes to `pos` and double-clicks there.
    fn double_click(&mut self, pos: egui::Pos2) {
        self.h.event(egui::Event::PointerMoved(pos));
        self.hold(0.5);
        for pressed in [true, false, true, false] {
            self.h.input_mut().events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        }
        self.frame();
    }

    /// Frames in real time, for the tasks of the app, until `done`.
    fn wait(&mut self, what: &str, done: impl Fn(&App) -> bool) {
        let start = Instant::now();
        while !done(&self.h.state().app) {
            assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
            let frame = Instant::now();
            self.frame();
            std::thread::sleep(Duration::from_secs_f32(1.0 / FPS).saturating_sub(frame.elapsed()));
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("frames dir"));
    let port: u16 = args.next().and_then(|p| p.parse().ok()).expect("port of the demo server");
    std::fs::create_dir_all(&dir).expect("frames dir");

    // Three connections, the first one used last: two Down go to the demo server.
    let config = PathBuf::from(std::env::var_os("RUSTYROBO_CONFIG_DIR").expect("RUSTYROBO_CONFIG_DIR"));
    std::fs::create_dir_all(&config).expect("config dir");
    let ago = |hours| Some(chrono::Utc::now() - chrono::Duration::hours(hours));
    let conn = |name: &str, host: &str, port: u16, last| ConnectionSettings { name: name.into(), hosts: vec![HostPort { host: host.into(), port }], last_connected: last, ..Default::default() };
    let settings = AppSettings {
        connections: vec![conn("Local", "127.0.0.1", 27017, ago(2)), conn("Staging", "staging.example.net", 27017, ago(72)), conn("Demo", "127.0.0.1", port, ago(240))],
        ..Default::default()
    };
    std::fs::write(config.join("settings.json"), serde_json::to_string(&settings).unwrap()).expect("settings");

    let h = Harness::builder()
        .with_size(egui::vec2(960.0, 720.0))
        .with_step_dt(1.0 / FPS)
        .wgpu()
        .build_eframe(|cc| Demo { app: App::new(cc), keys: Vec::new(), since: f64::NEG_INFINITY });
    let mut r = Recorder { h, dir, frames: 0 };

    // The connection manager, at startup.
    r.hold(1.2);
    r.press(Modifiers::NONE, Key::ArrowDown, &[icon::ARROW_DOWN]);
    r.hold(0.5);
    r.press(Modifiers::NONE, Key::ArrowDown, &[icon::ARROW_DOWN]);
    r.hold(0.8);
    r.press(Modifiers::NONE, Key::Enter, &["Enter"]);
    r.wait("the connection", |app| app.servers.first().is_some_and(|s| s.conn().is_some()));
    r.hold(1.0);

    // Ctrl+P: the collection by its name.
    r.press(Modifiers::COMMAND, Key::P, &["Ctrl", "P"]);
    r.hold(0.5);
    r.type_text("orders");
    let loaded = |app: &App| app.servers.iter().flat_map(|s| &s.dbs).any(|d| matches!(&d.collections, Load::Ready(c) if c.iter().any(|c| c.name == "orders")));
    r.wait("the collections", loaded);
    r.hold(0.7);
    r.press(Modifiers::NONE, Key::Enter, &["Enter"]);
    r.wait("the documents", |app| app.tabs.iter().any(|t| t.outputs.first().is_some_and(|o| !o.docs().is_empty())));
    r.hold(1.2);

    // Alt+Down: the results take the keyboard; E edits the selected document.
    r.press(Modifiers::ALT, Key::ArrowDown, &["Alt", icon::ARROW_DOWN]);
    r.hold(1.3);
    r.press(Modifiers::NONE, Key::E, &["E"]);
    r.hold(1.2);

    // A value changed in the editor, saved with Ctrl+S: the page comes again,
    // the same row selected.
    let at = r.text_pos("\"status\" : \"", "paid");
    r.double_click(at);
    r.hold(0.4);
    r.h.event(egui::Event::PointerGone);
    r.type_text("shipped");
    r.hold(0.6);
    r.press(Modifiers::COMMAND, Key::S, &["Ctrl", "S"]);
    let status = |app: &App, doc: usize| {
        let docs = app.tabs.iter().find_map(|t| t.outputs.first())?.docs();
        docs.get(doc)?.as_document()?.get_str("status").ok().map(str::to_string)
    };
    r.wait("the saved document", |app| app.dialog.is_none() && status(app, 0).as_deref() == Some("shipped"));
    r.hold(1.0);

    // Two documents down, opened; a value edited in place.
    for _ in 0..2 {
        r.press(Modifiers::NONE, Key::ArrowDown, &[icon::ARROW_DOWN]);
        r.hold(0.45);
    }
    r.press(Modifiers::NONE, Key::ArrowRight, &[icon::ARROW_RIGHT]);
    r.hold(0.8);
    for _ in 0..3 {
        r.press(Modifiers::NONE, Key::ArrowDown, &[icon::ARROW_DOWN]);
        r.hold(0.35);
    }
    r.hold(0.3);
    r.press(Modifiers::NONE, Key::Enter, &["Enter"]);
    r.hold(0.6);
    r.press(Modifiers::COMMAND, Key::A, &["Ctrl", "A"]);
    r.hold(0.4);
    r.type_text("\"returned\"");
    r.hold(0.5);
    r.press(Modifiers::NONE, Key::Enter, &["Enter"]);
    r.wait("the value saved", |app| status(app, 2).as_deref() == Some("returned"));
    r.hold(1.0);

    // An array: its document opens on it, its lines shown; a value of it
    // changed, saved again.
    for _ in 0..2 {
        r.press(Modifiers::NONE, Key::ArrowDown, &[icon::ARROW_DOWN]);
        r.hold(0.35);
    }
    r.hold(0.3);
    r.press(Modifiers::NONE, Key::Enter, &["Enter"]);
    r.hold(2.0);
    // The last value of an item: a double-click on a number before a comma takes it too.
    let at = r.text_pos("\"price\" : ", "8");
    r.double_click(at);
    r.hold(0.4);
    r.h.event(egui::Event::PointerGone);
    r.type_text("9.5");
    r.hold(0.8);
    r.press(Modifiers::COMMAND, Key::S, &["Ctrl", "S"]);
    let price = |app: &App| {
        let docs = app.tabs.iter().find_map(|t| t.outputs.first())?.docs();
        docs.get(2)?.as_document()?.get_array("items").ok()?.first()?.as_document()?.get("price")?.as_f64()
    };
    r.wait("the array saved", |app| app.dialog.is_none() && price(app) == Some(9.5));
    r.hold(1.5);
    println!("{} frames", r.frames);
}
