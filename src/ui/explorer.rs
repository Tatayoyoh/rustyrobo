//! Database Explorer: servers → databases → collections / functions / users
//! → indexes, with the same context menus as Robo 3T.

use std::cell::RefCell;

use bson::{Bson, Document, doc};
use egui::collapsing_header::CollapsingState;
use egui::{Id, Response, RichText, Ui};

use crate::app::{Action, App, Load, Refresh, ServerStatus};
use crate::ui::dialogs::{Dialog, DocMode};
use crate::ui::{ICON_COLLECTION, ICON_DB, ICON_FOLDER, ICON_FUNCTION, ICON_INDEX, ICON_SERVER, ICON_USER, ICON_VIEW, color_dot, icon};

const SYSTEM_DBS: &[&str] = &["admin", "config", "local"];

/// `db.getCollection('name')` with quotes and backslashes escaped.
pub fn coll_ref(name: &str) -> String {
    format!("db.getCollection('{}')", name.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn selected_id(ui: &Ui) -> Option<Id> {
    ui.data(|d| d.get_temp::<Id>(Id::new("explorer_selected")))
}

/// Selectable row; double-click is reported, selection stored in memory.
fn row(ui: &mut Ui, id: Id, text: RichText) -> Response {
    let sel = selected_id(ui) == Some(id);
    let r = ui.add(egui::Button::selectable(sel, text).frame_when_inactive(false));
    if r.clicked() || r.secondary_clicked() {
        ui.data_mut(|d| d.insert_temp(Id::new("explorer_selected"), id));
    }
    r
}

/// Collapsible node. `header` returns the row response; double-clicking
/// the row toggles the node.
fn node(ui: &mut Ui, id: Id, header: impl FnOnce(&mut Ui) -> Response, body: impl FnOnce(&mut Ui)) -> bool {
    let state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let mut toggle = false;
    let mut hr = state.show_header(ui, |ui| {
        let r = header(ui);
        toggle = r.double_clicked();
    });
    if toggle {
        hr.toggle();
    }
    let open = hr.is_open();
    hr.body(|ui| body(ui));
    open
}

fn leaf(ui: &mut Ui, id: Id, text: RichText) -> Response {
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().indent);
        row(ui, id, text)
    })
    .inner
}

fn loading_line(ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().indent);
        ui.spinner();
        ui.label(RichText::new("Loading…").weak());
    });
}

fn failed_line(ui: &mut Ui, err: &str, theme: &crate::theme::Theme) {
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().indent);
        ui.label(RichText::new(format!("{} ", icon::WARNING)).color(theme.error));
        ui.add(egui::Label::new(RichText::new(err).color(theme.error).small()).wrap());
    });
}

fn count<T>(l: &Load<Vec<T>>) -> String {
    match l {
        Load::Ready(v) => format!(" ({})", v.len()),
        _ => String::new(),
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.strong("Database Explorer");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button(icon::X).on_hover_text("Hide (Ctrl+E)").clicked() {
                app.settings.show_explorer = false;
            }
        });
    });
    ui.separator();
    let actions = RefCell::new(Vec::new());
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        if app.servers.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Not connected").weak());
                if ui.button(format!("{} Connect…", crate::ui::ICON_CONNECT)).clicked() {
                    actions.borrow_mut().push(Action::ShowConnections);
                }
            });
        }
        for s in &app.servers {
            server_node(app, s, ui, &actions);
        }
    });
    app.actions.extend(actions.into_inner());
}

fn server_node(app: &App, s: &crate::app::Server, ui: &mut Ui, actions: &RefCell<Vec<Action>>) {
    let th = &app.theme;
    let sid = s.id;
    let id = Id::new(("srv", sid));
    let header = |ui: &mut Ui| {
        ui.horizontal(|ui| {
            color_dot(ui, s.settings.color, 10.0);
            let (icon, color) = match &s.status {
                ServerStatus::Connecting => (ICON_SERVER, th.text_weak),
                ServerStatus::Connected(_) => (ICON_SERVER, th.text),
                ServerStatus::Failed(_) => (icon::WARNING, th.error),
            };
            let r = row(ui, id, RichText::new(format!("{icon} {}", s.label())).color(color).strong());
            if matches!(s.status, ServerStatus::Connecting) {
                ui.spinner();
            }
            let r = match (&s.status, s.conn()) {
                (ServerStatus::Failed(e), _) => r.on_hover_text(e),
                (_, Some(c)) => r.on_hover_text(format!(
                    "{}\nMongoDB {}{}{}",
                    s.settings.address_label(),
                    c.version,
                    c.set_name.as_ref().map(|n| format!("\nReplica set: {n}")).unwrap_or_default(),
                    if c.tunnel.is_some() { "\nvia SSH tunnel" } else { "" }
                )),
                _ => r,
            };
            r.context_menu(|ui| server_menu(app, s, ui, actions));
            r
        })
        .inner
    };
    let open = node(ui, id, header, |ui| {
        if s.dbs_loading {
            loading_line(ui);
        }
        if let ServerStatus::Failed(e) = &s.status {
            failed_line(ui, e, th);
            return;
        }
        let system: Vec<_> = s.dbs.iter().filter(|d| SYSTEM_DBS.contains(&d.name.as_str())).collect();
        if !system.is_empty() {
            node(
                ui,
                id.with("system"),
                |ui| row(ui, id.with("system"), RichText::new(format!("{ICON_FOLDER} System"))),
                |ui| {
                    for d in system {
                        db_node(app, sid, d, ui, actions);
                    }
                },
            );
        }
        for d in s.dbs.iter().filter(|d| !SYSTEM_DBS.contains(&d.name.as_str())) {
            db_node(app, sid, d, ui, actions);
        }
    });
    let _ = open;
}

fn server_menu(app: &App, s: &crate::app::Server, ui: &mut Ui, actions: &RefCell<Vec<Action>>) {
    let sid = s.id;
    let item = |ui: &mut Ui, label: &str, a: Action| {
        if ui.button(label).clicked() {
            actions.borrow_mut().push(a);
            ui.close();
        }
    };
    let shell = |script: &str, exec: bool| Action::OpenShell {
        server: sid,
        db: String::new(),
        script: script.to_string(),
        execute: exec,
        title: String::new(),
        cursor: None,
    };
    if s.conn().is_none() {
        item(ui, "Reconnect", Action::RefreshServer(sid));
        item(ui, "Close", Action::Disconnect(sid));
        return;
    }
    item(ui, "Create Database…", Action::Dialog(Dialog::CreateDatabase { server: sid, name: String::new(), collection: "collection".into() }));
    ui.separator();
    item(ui, "Server Status", shell("db.serverStatus()", true));
    item(ui, "Host Info", shell("db.hostInfo()", true));
    item(ui, "MongoDB Version", shell("db.version()", true));
    if s.conn().is_some_and(|c| c.set_name.is_some()) {
        item(ui, "Status of Replica Set", shell("rs.status()", true));
    }
    ui.separator();
    item(ui, "Show Log", shell("show log", true));
    item(ui, "Current Operations", shell("db.currentOp()", true));
    ui.separator();
    item(ui, "Open Shell", shell("", false));
    item(ui, "Refresh", Action::RefreshServer(sid));
    item(ui, "Disconnect", Action::Disconnect(sid));
    let _ = app;
}

fn db_node(app: &App, sid: u64, d: &crate::app::DbNode, ui: &mut Ui, actions: &RefCell<Vec<Action>>) {
    let th = &app.theme;
    let id = Id::new(("db", sid, &d.name));
    let db = d.name.clone();
    let shell = |script: &str, exec: bool, cursor: Option<usize>| Action::OpenShell {
        server: sid,
        db: db.clone(),
        script: script.to_string(),
        execute: exec,
        title: String::new(),
        cursor,
    };
    node(
        ui,
        id,
        |ui| {
            let r = row(ui, id, RichText::new(format!("{ICON_DB} {}", d.name)));
            r.context_menu(|ui| {
                let item = |ui: &mut Ui, label: &str, a: Action| {
                    if ui.button(label).clicked() {
                        actions.borrow_mut().push(a);
                        ui.close();
                    }
                };
                item(ui, "Open Shell", shell("", false, None));
                item(ui, "Refresh", Action::LoadCollections(sid, db.clone()));
                ui.separator();
                item(ui, "Create Collection…", Action::Dialog(Dialog::create_collection(sid, &db)));
                item(ui, "Add User…", Action::Dialog(Dialog::new_user(sid, &db)));
                item(ui, "Add Function…", Action::Dialog(Dialog::Function { server: sid, db: db.clone(), original: None, name: String::new(), code: "function () {\n    \n}".into(), error: None }));
                ui.separator();
                item(ui, "Database Statistics", shell("db.stats()", true, None));
                item(ui, "Current Operations", shell("db.currentOp()", true, None));
                item(ui, "Kill Operation…", shell("db.killOp()", false, Some(10)));
                item(ui, "Repair Database…", shell("db.repairDatabase()", false, None));
                ui.separator();
                item(
                    ui,
                    "Drop Database…",
                    Action::Dialog(Dialog::confirm(
                        "Drop Database",
                        format!("Drop database \"{db}\"?\n\nAll collections, indexes and users of this database will be permanently deleted."),
                        Action::Command { server: sid, db: db.clone(), cmd: doc! { "dropDatabase": 1 }, label: format!("Drop database {db}"), refresh: Refresh::Databases },
                    )),
                );
            });
            r
        },
        |ui| {
            // Collections
            let cid = id.with("collections");
            node(
                ui,
                cid,
                |ui| {
                    let r = row(ui, cid, RichText::new(format!("{ICON_FOLDER} Collections{}", count(&d.collections))));
                    r.context_menu(|ui| {
                        if ui.button("Create Collection…").clicked() {
                            actions.borrow_mut().push(Action::Dialog(Dialog::create_collection(sid, &db)));
                            ui.close();
                        }
                        if ui.button("Collections Statistics").clicked() {
                            actions.borrow_mut().push(shell("db.printCollectionStats()", true, None));
                            ui.close();
                        }
                        if ui.button("Refresh").clicked() {
                            actions.borrow_mut().push(Action::LoadCollections(sid, db.clone()));
                            ui.close();
                        }
                    });
                    r
                },
                |ui| match &d.collections {
                    Load::Idle => {
                        actions.borrow_mut().push(Action::LoadCollections(sid, db.clone()));
                        loading_line(ui);
                    }
                    Load::Loading => loading_line(ui),
                    Load::Failed(e) => failed_line(ui, e, th),
                    Load::Ready(list) => {
                        let (system, user): (Vec<_>, Vec<_>) = list.iter().partition(|c| c.name.starts_with("system."));
                        if !system.is_empty() {
                            node(
                                ui,
                                cid.with("system"),
                                |ui| row(ui, cid.with("system"), RichText::new(format!("{ICON_FOLDER} System"))),
                                |ui| {
                                    for c in system {
                                        coll_node(app, sid, &db, c, ui, actions);
                                    }
                                },
                            );
                        }
                        for c in user {
                            coll_node(app, sid, &db, c, ui, actions);
                        }
                    }
                },
            );

            // Functions
            let fid = id.with("functions");
            node(
                ui,
                fid,
                |ui| {
                    let r = row(ui, fid, RichText::new(format!("{ICON_FOLDER} Functions{}", count(&d.functions))));
                    r.context_menu(|ui| {
                        if ui.button("Add Function…").clicked() {
                            actions.borrow_mut().push(Action::Dialog(Dialog::Function { server: sid, db: db.clone(), original: None, name: String::new(), code: "function () {\n    \n}".into(), error: None }));
                            ui.close();
                        }
                        if ui.button("View Functions").clicked() {
                            actions.borrow_mut().push(shell("db.system.js.find()", true, None));
                            ui.close();
                        }
                        if ui.button("Refresh").clicked() {
                            actions.borrow_mut().push(Action::LoadFunctions(sid, db.clone()));
                            ui.close();
                        }
                    });
                    r
                },
                |ui| match &d.functions {
                    Load::Idle => {
                        actions.borrow_mut().push(Action::LoadFunctions(sid, db.clone()));
                        loading_line(ui);
                    }
                    Load::Loading => loading_line(ui),
                    Load::Failed(e) => failed_line(ui, e, th),
                    Load::Ready(list) => {
                        for f in list {
                            let name = f.get("_id").map(|v| match v {
                                Bson::String(s) => s.clone(),
                                other => other.to_string(),
                            }).unwrap_or_default();
                            let code = match f.get("value") {
                                Some(Bson::JavaScriptCode(c)) => c.clone(),
                                Some(Bson::JavaScriptCodeWithScope(c)) => c.code.clone(),
                                Some(other) => other.to_string(),
                                None => String::new(),
                            };
                            let lid = fid.with(&name);
                            let r = leaf(ui, lid, RichText::new(format!("{ICON_FUNCTION} {name}")));
                            let edit = Action::Dialog(Dialog::Function { server: sid, db: db.clone(), original: Some(name.clone()), name: name.clone(), code, error: None });
                            if r.double_clicked() {
                                actions.borrow_mut().push(edit.clone());
                            }
                            r.context_menu(|ui| {
                                if ui.button("Edit Function…").clicked() {
                                    actions.borrow_mut().push(edit.clone());
                                    ui.close();
                                }
                                if ui.button("Remove Function…").clicked() {
                                    actions.borrow_mut().push(Action::Dialog(Dialog::confirm(
                                        "Remove Function",
                                        format!("Remove function \"{name}\"?"),
                                        Action::Command {
                                            server: sid,
                                            db: db.clone(),
                                            cmd: doc! { "delete": "system.js", "deletes": [{ "q": { "_id": &name }, "limit": 1 }] },
                                            label: format!("Remove function {name}"),
                                            refresh: Refresh::Functions(db.clone()),
                                        },
                                    )));
                                    ui.close();
                                }
                            });
                        }
                    }
                },
            );

            // Users
            let uid = id.with("users");
            node(
                ui,
                uid,
                |ui| {
                    let r = row(ui, uid, RichText::new(format!("{ICON_FOLDER} Users{}", count(&d.users))));
                    r.context_menu(|ui| {
                        if ui.button("Add User…").clicked() {
                            actions.borrow_mut().push(Action::Dialog(Dialog::new_user(sid, &db)));
                            ui.close();
                        }
                        if ui.button("View Users").clicked() {
                            actions.borrow_mut().push(shell("db.getUsers()", true, None));
                            ui.close();
                        }
                        if ui.button("Refresh").clicked() {
                            actions.borrow_mut().push(Action::LoadUsers(sid, db.clone()));
                            ui.close();
                        }
                    });
                    r
                },
                |ui| match &d.users {
                    Load::Idle => {
                        actions.borrow_mut().push(Action::LoadUsers(sid, db.clone()));
                        loading_line(ui);
                    }
                    Load::Loading => loading_line(ui),
                    Load::Failed(e) => failed_line(ui, e, th),
                    Load::Ready(list) => {
                        for u in list {
                            let name = u.get_str("user").unwrap_or_default().to_string();
                            let lid = uid.with(&name);
                            let r = leaf(ui, lid, RichText::new(format!("{ICON_USER} {name}")));
                            let view = shell(&format!("db.getUser({})", serde_json::to_string(&name).unwrap_or_default()), true, None);
                            if r.double_clicked() {
                                actions.borrow_mut().push(view.clone());
                            }
                            r.context_menu(|ui| {
                                if ui.button("View User").clicked() {
                                    actions.borrow_mut().push(view.clone());
                                    ui.close();
                                }
                                if ui.button("Edit User…").clicked() {
                                    actions.borrow_mut().push(Action::Dialog(Dialog::edit_user(sid, &db, u)));
                                    ui.close();
                                }
                                if ui.button("Drop User…").clicked() {
                                    actions.borrow_mut().push(Action::Dialog(Dialog::confirm(
                                        "Drop User",
                                        format!("Drop user \"{name}\" from database \"{db}\"?"),
                                        Action::Command { server: sid, db: db.clone(), cmd: doc! { "dropUser": &name }, label: format!("Drop user {name}"), refresh: Refresh::Users(db.clone()) },
                                    )));
                                    ui.close();
                                }
                            });
                        }
                    }
                },
            );
        },
    );
}

fn coll_node(app: &App, sid: u64, db: &str, c: &crate::app::CollNode, ui: &mut Ui, actions: &RefCell<Vec<Action>>) {
    let th = &app.theme;
    let name = c.name.clone();
    let id = Id::new(("coll", sid, db, &name));
    let cref = coll_ref(&name);
    let shell = |postfix: &str, exec: bool, cursor: Option<usize>| Action::OpenShell {
        server: sid,
        db: db.to_string(),
        script: format!("{cref}.{postfix}"),
        execute: exec,
        title: name.clone(),
        cursor,
    };
    let is_view = c.kind == "view";
    let icon = if is_view { ICON_VIEW } else { ICON_COLLECTION };
    let view_docs = shell("find({})", true, Some(cref.len() + 7));
    node(
        ui,
        id,
        |ui| {
            let r = row(ui, id, RichText::new(format!("{icon} {name}")));
            let r = if c.kind != "collection" { r.on_hover_text(&c.kind) } else { r };
            if r.double_clicked() {
                actions.borrow_mut().push(view_docs.clone());
            }
            r.context_menu(|ui| {
                let item = |ui: &mut Ui, label: &str, a: Action| {
                    if ui.button(label).clicked() {
                        actions.borrow_mut().push(a);
                        ui.close();
                    }
                };
                item(ui, "View Documents", view_docs.clone());
                if !is_view {
                    item(ui, "Insert Document…", Action::Dialog(Dialog::document(DocMode::Insert, sid, db, &name, "{\n    \n}".into(), None, None)));
                    item(
                        ui,
                        "Update Documents…",
                        shell("updateMany(\n    // filter\n    { \"key\" : \"value\" },\n    // update\n    { $set: { \"key\" : \"new value\" } },\n    // options\n    { upsert: false }\n)", false, None),
                    );
                    item(ui, "Remove Documents…", shell("deleteMany({ \"key\" : \"value\" })", false, None));
                    item(
                        ui,
                        "Remove All Documents…",
                        Action::Dialog(Dialog::confirm(
                            "Remove All Documents",
                            format!("Remove all documents from collection \"{name}\"?"),
                            Action::Command { server: sid, db: db.to_string(), cmd: doc! { "delete": &name, "deletes": [{ "q": {}, "limit": 0 }] }, label: format!("Remove all documents from {name}"), refresh: Refresh::None },
                        )),
                    );
                    ui.separator();
                    item(ui, "Add Index…", Action::Dialog(Dialog::new_index(sid, db, &name)));
                    item(ui, "Rebuild Indexes…", shell("reIndex()", false, None));
                }
                ui.separator();
                item(ui, "Rename Collection…", Action::Dialog(Dialog::RenameCollection { server: sid, db: db.to_string(), coll: name.clone(), name: name.clone() }));
                if !is_view {
                    item(ui, "Duplicate Collection…", Action::Dialog(Dialog::DuplicateCollection { server: sid, db: db.to_string(), coll: name.clone(), name: format!("{name}_copy") }));
                    item(ui, "Copy Collection to Database…", Action::Dialog(Dialog::CopyCollection { server: sid, db: db.to_string(), coll: name.clone(), target_db: String::new(), target_name: name.clone() }));
                }
                item(
                    ui,
                    if is_view { "Drop View…" } else { "Drop Collection…" },
                    Action::Dialog(Dialog::confirm(
                        "Drop Collection",
                        format!("Drop collection \"{name}\"?\n\nAll documents and indexes will be permanently deleted."),
                        Action::Command { server: sid, db: db.to_string(), cmd: doc! { "drop": &name }, label: format!("Drop collection {name}"), refresh: Refresh::Collections(db.to_string()) },
                    )),
                );
                ui.separator();
                item(ui, "Statistics", shell("stats()", true, None));
                if !is_view {
                    item(ui, "Storage Size", shell("storageSize()", true, None));
                    item(ui, "Total Index Size", shell("totalIndexSize()", true, None));
                    item(ui, "Total Size", shell("totalSize()", true, None));
                    item(ui, "Shard Version", shell("getShardVersion()", true, None));
                    item(ui, "Shard Distribution", shell("getShardDistribution()", true, None));
                }
            });
            r
        },
        |ui| {
            if is_view {
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().indent);
                    ui.label(RichText::new("view").weak().italics());
                });
                return;
            }
            let iid = id.with("indexes");
            node(
                ui,
                iid,
                |ui| {
                    let r = row(ui, iid, RichText::new(format!("{ICON_FOLDER} Indexes{}", count(&c.indexes))));
                    r.context_menu(|ui| {
                        if ui.button("View Indexes").clicked() {
                            actions.borrow_mut().push(shell("getIndexes()", true, None));
                            ui.close();
                        }
                        if ui.button("Add Index…").clicked() {
                            actions.borrow_mut().push(Action::Dialog(Dialog::new_index(sid, db, &name)));
                            ui.close();
                        }
                        if ui.button("Rebuild Indexes…").clicked() {
                            actions.borrow_mut().push(shell("reIndex()", false, None));
                            ui.close();
                        }
                        if ui.button("Refresh").clicked() {
                            actions.borrow_mut().push(Action::LoadIndexes(sid, db.to_string(), name.clone()));
                            ui.close();
                        }
                    });
                    r
                },
                |ui| match &c.indexes {
                    Load::Idle => {
                        actions.borrow_mut().push(Action::LoadIndexes(sid, db.to_string(), name.clone()));
                        loading_line(ui);
                    }
                    Load::Loading => loading_line(ui),
                    Load::Failed(e) => failed_line(ui, e, th),
                    Load::Ready(list) => {
                        for ix in list {
                            index_leaf(sid, db, &name, ix, iid, ui, actions);
                        }
                    }
                },
            );
        },
    );
}

fn index_leaf(sid: u64, db: &str, coll: &str, ix: &Document, parent: Id, ui: &mut Ui, actions: &RefCell<Vec<Action>>) {
    let iname = ix.get_str("name").unwrap_or_default().to_string();
    let keys = ix.get_document("key").map(|k| crate::bsonfmt::scalar_shell(&Bson::Document(k.clone()), Default::default())).unwrap_or_default();
    let keys_one_line: String = keys.split_whitespace().collect::<Vec<_>>().join(" ");
    let r = leaf(ui, parent.with(&iname), RichText::new(format!("{ICON_INDEX} {iname}")));
    let r = r.on_hover_text(keys_one_line);
    let edit = Action::Dialog(Dialog::edit_index(sid, db, coll, ix));
    if r.double_clicked() && iname != "_id_" {
        actions.borrow_mut().push(edit.clone());
    }
    r.context_menu(|ui| {
        if ui.add_enabled(iname != "_id_", egui::Button::new("Edit Index…")).clicked() {
            actions.borrow_mut().push(edit.clone());
            ui.close();
        }
        if ui.add_enabled(iname != "_id_", egui::Button::new("Drop Index…")).clicked() {
            actions.borrow_mut().push(Action::Dialog(Dialog::confirm(
                "Drop Index",
                format!("Drop index \"{iname}\" of collection \"{coll}\"?"),
                Action::Command {
                    server: sid,
                    db: db.to_string(),
                    cmd: doc! { "dropIndexes": coll, "index": &iname },
                    label: format!("Drop index {iname}"),
                    refresh: Refresh::Indexes(db.to_string(), coll.to_string()),
                },
            )));
            ui.close();
        }
    });
}
