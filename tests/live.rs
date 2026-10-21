//! Live tests against real servers. Run with:
//!   RR_LIVE=1 cargo test --test live -- --nocapture --test-threads=1
//! Expects the docker setup described in README (mongo 8 on 27117 with
//! admin/secret, mongo 4.2 on 27142, openssh on 2222 tunnel/tunnelpw).

use std::sync::Arc;
use std::time::Duration;

use rustyrobo::mongo::{ConnectSecrets, Connection};
use rustyrobo::settings::{ConnectionSettings, Credentials, HostPort, SshAuthMethod};
use rustyrobo::shell::{OutputKind, ShellHandle, ShellOptions};

fn live() -> bool {
    if std::env::var("RR_LIVE").is_err() {
        return false;
    }
    // Keep test artefacts (learned SSH host keys…) out of the user's config.
    let dir = std::env::temp_dir().join(format!("rustyrobo-live-{}", std::process::id()));
    // SAFETY: set before any thread of this test reads the environment.
    unsafe { std::env::set_var("RUSTYROBO_CONFIG_DIR", dir) };
    true
}

fn conn8() -> ConnectionSettings {
    ConnectionSettings {
        hosts: vec![HostPort { host: "127.0.0.1".into(), port: 27117 }],
        credentials: Credentials {
            enabled: true,
            database: "admin".into(),
            username: "admin".into(),
            password: "secret".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn run_shell(rt: &tokio::runtime::Runtime, c: Arc<Connection>, script: &str) -> Vec<rustyrobo::shell::ShellOutput> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut sh = ShellHandle::spawn(
        c,
        rt.handle().clone(),
        "rrtest".into(),
        ShellOptions { batch_size: 50, timeout: Duration::from_secs(20), load_mongorc: false },
        move |r| {
            let _ = tx.send(r);
        },
    );
    sh.execute(script.into());
    rx.recv_timeout(Duration::from_secs(30)).unwrap().outputs
}

#[test]
fn modern_server_and_shell() {
    if !live() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let s = conn8();
    let c = rt
        .block_on(Connection::open(&s, &ConnectSecrets::default(), |st| println!("{st:?}")))
        .unwrap();
    assert!(!c.is_legacy());
    println!("version {} wire {}", c.version, c.wire_version);
    let c = Arc::new(c);
    let outs = run_shell(
        &rt,
        c.clone(),
        r#"db.items.drop()
for (var i = 0; i < 120; i++) { db.items.insertOne({ n: i, tag: i % 3 == 0 ? "fizz" : "x", at: ISODate("2024-01-01T00:00:00Z"), l: NumberLong(i) }) }
db.items.countDocuments({})
db.getCollection('items').find({ tag: "fizz" }).sort({ n: -1 })
db.items.aggregate([{ $group: { _id: "$tag", c: { $sum: 1 } } }, { $sort: { _id: 1 } }])
print("hello"); printjson({ a: ObjectId("5f1d7a0e1c9d440000a1b2c3") })
db.items.updateMany({ tag: "x" }, { $set: { seen: true } })
const total = db.items.find().toArray().length
total
show collections
use other
db.getName()
db.nope.find({ $bad: 1 })
db.never.find()"#,
    );
    for o in &outs {
        println!("--- [{}] {:?}", o.statement.lines().next().unwrap_or(""), match &o.kind {
            OutputKind::Docs { docs, spec, more, .. } => format!("{} docs, spec={}, more={more}", docs.len(), spec.is_some()),
            k => format!("{k:?}"),
        });
    }
    let texts: Vec<String> = outs
        .iter()
        .filter_map(|o| match &o.kind {
            OutputKind::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert!(texts.contains(&"120".to_string()));
    assert!(texts.iter().any(|t| t.contains("hello")));
    assert!(texts.contains(&"switched to db other".to_string()));
    let fizz = outs.iter().find(|o| o.statement.contains("fizz\" }).sort")).unwrap();
    match &fizz.kind {
        OutputKind::Docs { docs, spec, more, .. } => {
            assert_eq!(docs.len(), 40);
            assert!(!more);
            let spec = spec.as_ref().unwrap();
            assert_eq!(spec.coll, "items");
            let page = rt.block_on(rustyrobo::shell::fetch_page(&c, spec, 10, 5)).unwrap();
            assert_eq!(page.len(), 5);
        }
        k => panic!("{k:?}"),
    }
    assert!(matches!(outs.last().unwrap().kind, OutputKind::Error(_)));
}

#[test]
fn legacy_server() {
    if !live() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let s = ConnectionSettings { hosts: vec![HostPort { host: "127.0.0.1".into(), port: 27142 }], ..Default::default() };
    let c = rt
        .block_on(Connection::open(&s, &ConnectSecrets::default(), |st| println!("{st:?}")))
        .unwrap();
    assert!(c.is_legacy(), "4.2 should use the legacy backend");
    let outs = run_shell(&rt, Arc::new(c), "db.t.insertMany([{a:1},{a:2}])\ndb.t.find().toArray().length\ndb.t.drop()");
    for o in &outs {
        println!("{:?}", o.kind);
    }
    assert!(outs.iter().all(|o| !matches!(o.kind, OutputKind::Error(_))));
}

#[test]
fn ssh_tunnel() {
    if !live() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut s = conn8();
    // Inside the docker network the mongo container is reachable by name.
    s.hosts = vec![HostPort { host: "rr-mongo8".into(), port: 27017 }];
    s.ssh.enabled = true;
    s.ssh.host = "127.0.0.1".into();
    s.ssh.port = 2222;
    s.ssh.username = "tunnel".into();
    s.ssh.method = SshAuthMethod::Password;
    s.ssh.password = "tunnelpw".into();
    let c = rt
        .block_on(Connection::open(&s, &ConnectSecrets::default(), |st| println!("{st:?}")))
        .unwrap();
    assert!(c.tunnel.is_some());
    assert!(!c.databases.is_empty());
    println!("databases via tunnel: {:?}", c.databases);
}
