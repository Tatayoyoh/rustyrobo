//! End-to-end tests against real servers. Run with:
//!   docker compose -f dev/docker-compose.yml up -d --wait
//!   RR_LIVE=1 cargo test --test live -- --test-threads=1
//! Servers: MongoDB 8 on 27117 (admin/secret), MongoDB 4.2 on 27142 (no
//! auth), OpenSSH on 2222 (tunnel/tunnelpw, keys in dev/ssh).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bson::{Bson, doc};
use rustyrobo::mongo::{ConnectSecrets, Connection, StepKind};
use rustyrobo::settings::{ConnectionSettings, Credentials, HostPort, SshAuthMethod};
use rustyrobo::shell::{OutputKind, ShellHandle, ShellOptions, ShellOutput, fetch_page};

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

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn mongo8() -> ConnectionSettings {
    ConnectionSettings {
        hosts: vec![HostPort { host: "127.0.0.1".into(), port: 27117 }],
        credentials: Credentials {
            enabled: true,
            database: "admin".into(),
            username: "admin".into(),
            password: "secret".into(),
            ..Default::default()
        },
        connect_timeout_secs: 5,
        ..Default::default()
    }
}

fn mongo42() -> ConnectionSettings {
    ConnectionSettings {
        hosts: vec![HostPort { host: "127.0.0.1".into(), port: 27142 }],
        connect_timeout_secs: 5,
        ..Default::default()
    }
}

fn via_ssh(method: SshAuthMethod) -> ConnectionSettings {
    let mut s = mongo8();
    // Inside the docker network the mongo container is reachable by name.
    s.hosts = vec![HostPort { host: "rr-mongo8".into(), port: 27017 }];
    s.ssh.enabled = true;
    s.ssh.host = "127.0.0.1".into();
    s.ssh.port = 2222;
    s.ssh.username = "tunnel".into();
    s.ssh.method = method;
    s.ssh.password = "tunnelpw".into();
    s
}

fn key(name: &str) -> String {
    format!("{}/dev/ssh/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn open(rt: &tokio::runtime::Runtime, s: &ConnectionSettings) -> anyhow::Result<Connection> {
    rt.block_on(Connection::open(s, &ConnectSecrets::default(), |_| {}))
}

struct Shell {
    handle: ShellHandle,
    rx: crossbeam_channel::Receiver<rustyrobo::shell::ShellReply>,
}

impl Shell {
    fn new(rt: &tokio::runtime::Runtime, c: Arc<Connection>, db: &str, timeout: Duration) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let handle = ShellHandle::spawn(
            c,
            rt.handle().clone(),
            db.into(),
            ShellOptions { batch_size: 50, timeout, load_mongorc: false },
            move |r| {
                let _ = tx.send(r);
            },
        );
        Shell { handle, rx }
    }

    fn run(&mut self, script: &str) -> Vec<ShellOutput> {
        self.handle.execute(script.into());
        self.rx.recv_timeout(Duration::from_secs(30)).expect("shell reply").outputs
    }

    /// Runs a script that must succeed and returns its last output.
    fn ok(&mut self, script: &str) -> OutputKind {
        let outs = self.run(script);
        for o in &outs {
            if let OutputKind::Error(e) = &o.kind {
                panic!("`{}` failed: {e}", o.statement);
            }
        }
        outs.last().map(|o| o.kind.clone()).unwrap_or(OutputKind::Text(String::new()))
    }

    fn text(&mut self, script: &str) -> String {
        match self.ok(script) {
            OutputKind::Text(t) => t,
            other => panic!("`{script}`: expected text, got {other:?}"),
        }
    }

    fn docs(&mut self, script: &str) -> Vec<Bson> {
        match self.ok(script) {
            OutputKind::Docs { docs, .. } => docs,
            other => panic!("`{script}`: expected documents, got {other:?}"),
        }
    }

    fn error(&mut self, script: &str) -> String {
        let outs = self.run(script);
        match outs.last().map(|o| &o.kind) {
            Some(OutputKind::Error(e)) => e.clone(),
            other => panic!("`{script}`: expected an error, got {other:?}"),
        }
    }
}

fn field<'a>(d: &'a Bson, k: &str) -> &'a Bson {
    d.as_document().and_then(|d| d.get(k)).unwrap_or_else(|| panic!("missing field {k} in {d:?}"))
}

/// The shell API, exercised identically on every server version.
fn shell_suite(rt: &tokio::runtime::Runtime, s: &ConnectionSettings, db: &str) {
    let c = Arc::new(open(rt, s).expect("connect"));
    let mut sh = Shell::new(rt, c.clone(), db, Duration::from_secs(20));

    // Clean slate.
    sh.ok("db.dropDatabase()");

    // --- inserts
    let r = sh.docs("db.people.insertOne({ name: 'Ada', age: 36, tags: ['math'] })");
    assert_eq!(field(&r[0], "acknowledged"), &Bson::Boolean(true));
    assert!(matches!(field(&r[0], "insertedId"), Bson::ObjectId(_)));
    sh.ok("db.people.insertMany([{ name: 'Linus', age: 28 }, { name: 'Grace', age: 85, tags: ['navy', 'cobol'] }, { _id: 'custom', name: 'Ken', age: 80 }])");
    assert_eq!(sh.text("db.people.countDocuments({})"), "4");
    assert_eq!(sh.text("db.people.count({ age: { $gt: 50 } })"), "2");
    assert_eq!(sh.text("db.people.estimatedDocumentCount()"), "4");

    // --- queries: projection, sort, skip, limit, findOne
    let d = sh.docs("db.people.find({}, { _id: 0, name: 1 }).sort({ age: -1 }).skip(1).limit(2)");
    assert_eq!(d, vec![Bson::Document(doc! { "name": "Ken" }), Bson::Document(doc! { "name": "Ada" })]);
    let d = sh.docs("db.people.findOne({ name: 'Grace' })");
    assert_eq!(field(&d[0], "age"), &Bson::Int32(85));
    assert_eq!(sh.text("db.people.findOne({ name: 'Nobody' })"), "null");
    assert_eq!(sh.text("db.people.find({ _id: 'custom' }).toArray()[0].name"), "Ken");
    assert_eq!(sh.text("db.getCollection('people').find({ tags: 'cobol' }).count()"), "1");
    assert_eq!(sh.text("tojson(db.people.distinct('name').sort())"), "[\n    \"Ada\",\n    \"Grace\",\n    \"Ken\",\n    \"Linus\"\n]");

    // --- updates
    let r = sh.docs("db.people.updateOne({ name: 'Ada' }, { $set: { age: 37 }, $push: { tags: 'code' } })");
    assert_eq!(field(&r[0], "modifiedCount"), &Bson::Int32(1));
    let r = sh.docs("db.people.updateMany({ age: { $gt: 70 } }, { $set: { senior: true } })");
    assert_eq!(field(&r[0], "matchedCount"), &Bson::Int32(2));
    let r = sh.docs("db.people.updateOne({ name: 'Barbara' }, { $set: { age: 90 } }, { upsert: true })");
    assert_eq!(field(&r[0], "upsertedCount"), &Bson::Int32(1));
    sh.ok("db.people.replaceOne({ name: 'Barbara' }, { name: 'Barbara', age: 91 })");
    assert_eq!(sh.text("db.people.findOne({ name: 'Barbara' }).age"), "91");
    let d = sh.docs("db.people.findOneAndUpdate({ name: 'Linus' }, { $inc: { age: 1 } }, { returnNewDocument: true })");
    assert_eq!(field(&d[0], "age"), &Bson::Int32(29));
    assert!(sh.error("db.people.updateOne({}, { age: 1 })").contains("atomic operators"));
    // Legacy helpers still work.
    sh.ok("db.people.update({ name: 'Ken' }, { $set: { legacy: true } })");
    sh.ok("db.people.save({ _id: 'custom', name: 'Ken', age: 81 })");
    assert_eq!(sh.text("db.people.findOne({ _id: 'custom' }).age"), "81");

    // --- aggregation
    let d = sh.docs("db.people.aggregate([{ $match: { age: { $gte: 30 } } }, { $group: { _id: null, avg: { $avg: '$age' }, n: { $sum: 1 } } }])");
    assert_eq!(field(&d[0], "n"), &Bson::Int32(4));
    assert_eq!(sh.text("db.people.aggregate([{ $count: 'n' }]).toArray()[0].n"), "5");
    // The pipeline runs immediately even though no document is fetched yet.
    sh.run("var ignored = db.people.aggregate([{ $match: {} }, { $out: 'people_copy' }])");
    assert_eq!(sh.text("db.people_copy.countDocuments({})"), "5");
    sh.ok("db.people_copy.drop()");

    // --- bulkWrite and deletes
    sh.ok("db.people.bulkWrite([{ insertOne: { document: { name: 'Tmp1' } } }, { insertOne: { document: { name: 'Tmp2' } } }, { deleteOne: { filter: { name: 'Tmp1' } } }])");
    let r = sh.docs("db.people.deleteMany({ name: /^Tmp/ })");
    assert_eq!(field(&r[0], "deletedCount"), &Bson::Int32(1));
    sh.ok("db.people.deleteOne({ name: 'Barbara' })");
    assert_eq!(sh.text("db.people.countDocuments({})"), "4");

    // --- cursors: more documents than one batch, getMore through the session
    sh.ok("var docs = []; for (var i = 0; i < 230; i++) docs.push({ i: i }); db.many.insertMany(docs)");
    assert_eq!(sh.text("db.many.find().batchSize(20).toArray().length"), "230");
    assert_eq!(sh.text("var n = 0; db.many.find().forEach(function () { n++ }); n"), "230");
    assert_eq!(sh.text("db.many.find().sort({ i: -1 }).limit(3).map(function (d) { return d.i }).join(',')"), "229,228,227");
    assert_eq!(sh.text("db.many.aggregate([{ $match: {} }], { cursor: { batchSize: 10 } }).itcount()"), "230");
    assert_eq!(sh.text("db.many.find({ i: { $lt: 5 } }).size()"), "5");

    // First page + paging, like the result view.
    let outs = sh.run("db.many.find().sort({ i: 1 })");
    let OutputKind::Docs { docs, spec, more, .. } = &outs[0].kind else { panic!("{outs:?}") };
    assert_eq!(docs.len(), 50);
    assert!(*more);
    let spec = spec.clone().unwrap();
    let page = rt.block_on(fetch_page(&c, &spec, 200, 50)).unwrap();
    assert_eq!(page.len(), 30);
    assert_eq!(field(&page[0], "i"), &Bson::Int32(200));
    let outs = sh.run("db.many.find().skip(10).limit(5)");
    let OutputKind::Docs { spec, .. } = &outs[0].kind else { panic!() };
    // Paging never goes past the user's limit.
    assert_eq!(rt.block_on(fetch_page(&c, spec.as_ref().unwrap(), 12, 50)).unwrap().len(), 3);

    // --- BSON types round trip through the shell
    sh.ok(r#"db.types.insertOne({ _id: 1, long: NumberLong('9007199254740993'), int: NumberInt(7), dbl: 1.5,
        dec: NumberDecimal('123.456'), date: ISODate('2024-02-03T04:05:06.789Z'), uuid: UUID('0123456789abcdef0123456789abcdef'),
        bin: BinData(0, 'AQID'), re: /ab+c/i, ts: Timestamp(10, 2), min: MinKey, max: MaxKey, nul: null, arr: [1, 'a', { x: true }] })"#);
    let raw = rt
        .block_on(c.run_command(db, doc! { "find": "types", "filter": { "_id": 1 } }, None))
        .unwrap();
    let t = raw.get_document("cursor").unwrap().get_array("firstBatch").unwrap()[0].as_document().unwrap().clone();
    assert_eq!(t.get("long"), Some(&Bson::Int64(9007199254740993)));
    assert_eq!(t.get("int"), Some(&Bson::Int32(7)));
    assert_eq!(t.get("dbl"), Some(&Bson::Double(1.5)));
    assert_eq!(t.get("dec").map(|d| d.to_string()), Some("123.456".into()));
    assert!(matches!(t.get("date"), Some(Bson::DateTime(d)) if d.timestamp_millis() == 1706933106789));
    assert!(matches!(t.get("uuid"), Some(Bson::Binary(b)) if b.subtype == bson::spec::BinarySubtype::Uuid));
    assert!(matches!(t.get("re"), Some(Bson::RegularExpression(r)) if r.pattern == "ab+c" && r.options == "i"));
    assert!(matches!(t.get("ts"), Some(Bson::Timestamp(ts)) if ts.time == 10 && ts.increment == 2));
    assert_eq!(t.get("min"), Some(&Bson::MinKey));
    assert_eq!(t.get("max"), Some(&Bson::MaxKey));
    // And back into JavaScript values.
    assert_eq!(sh.text("var t = db.types.findOne(); [t.long.toString(), t.date.toISOString(), t.uuid.tojson(), t.re.source].join('|')"),
        "9007199254740993|2024-02-03T04:05:06.789Z|UUID(\"01234567-89ab-cdef-0123-456789abcdef\")|ab+c");

    // --- indexes
    assert_eq!(sh.text("db.people.createIndex({ name: 1 }, { unique: true })"), "name_1");
    assert_eq!(sh.text("db.people.getIndexes().length"), "2");
    assert!(sh.error("db.people.insertOne({ name: 'Ada' })").contains("duplicate key"));
    sh.ok("db.people.dropIndex('name_1')");
    assert_eq!(sh.text("db.people.getIndexKeys().length"), "1");

    // --- collections and database
    sh.ok("db.createCollection('capped', { capped: true, size: 4096 })");
    assert_eq!(sh.text("db.capped.isCapped()"), "true");
    sh.ok("db.capped.renameCollection('capped2')");
    assert!(sh.text("db.getCollectionNames().join(',')").contains("capped2"));
    assert_eq!(sh.text("db.capped2.drop()"), "true");
    // MongoDB 7.0+ no longer fails when dropping a missing collection.
    let missing = sh.text("db.nothere.drop()");
    assert!(missing == "false" || missing == "true", "{missing}");
    assert!(sh.text("db.people.stats().count").parse::<i64>().unwrap() >= 4);
    assert!(matches!(field(&sh.docs("db.stats()")[0], "db"), Bson::String(n) if n == db));
    assert!(!sh.text("db.version()").is_empty());
    assert!(sh.text("show collections").contains("people"));
    assert!(sh.text("show dbs").contains(db));

    // --- users
    sh.ok("db.createUser({ user: 'rr_test_user', pwd: 'pw', roles: [{ role: 'read', db: db.getName() }] })");
    assert_eq!(sh.text("db.getUser('rr_test_user').roles[0].role"), "read");
    sh.ok("db.dropUser('rr_test_user')");
    assert_eq!(sh.text("db.getUser('rr_test_user')"), "null");

    // --- shell behaviour: variables persist, use/print, multi statements
    sh.ok("var persisted = 41");
    assert_eq!(sh.text("persisted + 1"), "42");
    let outs = sh.run("print('one'); print('two')\nuse admin\ndb.getName()");
    let texts: Vec<String> = outs.iter().filter_map(|o| if let OutputKind::Text(t) = &o.kind { Some(t.clone()) } else { None }).collect();
    assert!(texts.contains(&"one".to_string()) && texts.contains(&"two".to_string()));
    assert!(texts.contains(&"switched to db admin".to_string()));
    assert_eq!(texts.last().unwrap(), "admin");
    sh.ok(&format!("use {db}"));
    // Execution stops at the first error, like the mongo shell.
    let outs = sh.run("db.people.find({ $bad: 1 })\nprint('not reached')");
    assert_eq!(outs.len(), 1);
    assert!(matches!(&outs[0].kind, OutputKind::Error(e) if e.contains("unknown top level operator")));
    // A statement split across lines by the heuristic is merged back.
    assert_eq!(sh.text("if (true)\n  print('merged')"), "merged");

    sh.ok("db.dropDatabase()");
}

#[test]
fn modern_server_shell_api() {
    if !live() {
        return;
    }
    let rt = rt();
    let c = open(&rt, &mongo8()).unwrap();
    assert!(!c.is_legacy());
    assert!(c.wire_version >= 9);
    drop(c);
    shell_suite(&rt, &mongo8(), "rr_e2e_modern");
}

#[test]
fn legacy_server_shell_api() {
    if !live() {
        return;
    }
    let rt = rt();
    let c = open(&rt, &mongo42()).unwrap();
    assert!(c.is_legacy(), "4.2 must use the legacy backend");
    assert!(c.version.starts_with("4.2"));
    drop(c);
    shell_suite(&rt, &mongo42(), "rr_e2e_legacy");
}

#[test]
fn stop_interrupts_a_running_script() {
    if !live() {
        return;
    }
    let rt = rt();
    let c = Arc::new(open(&rt, &mongo8()).unwrap());
    let mut sh = Shell::new(&rt, c, "rr_e2e_stop", Duration::from_secs(60));
    // A pure JavaScript loop is interrupted by the QuickJS interrupt handler.
    let started = Instant::now();
    sh.handle.execute("while (true) {}".into());
    std::thread::sleep(Duration::from_millis(300));
    sh.handle.interrupt();
    let reply = sh.rx.recv_timeout(Duration::from_secs(5)).expect("interrupted in time");
    assert!(matches!(&reply.outputs[0].kind, OutputKind::Error(e) if e.contains("interrupted")), "{:?}", reply.outputs);
    assert!(started.elapsed() < Duration::from_secs(5));
    // sleep() also honours the interruption.
    sh.handle.execute("sleep(20000); 1".into());
    std::thread::sleep(Duration::from_millis(200));
    sh.handle.interrupt();
    let reply = sh.rx.recv_timeout(Duration::from_secs(5)).expect("sleep interrupted");
    assert!(reply.elapsed < Duration::from_secs(5));
    // The shell stays usable afterwards.
    assert_eq!(sh.text("1 + 1"), "2");
}

#[test]
fn shell_timeout_aborts_slow_operations() {
    if !live() {
        return;
    }
    let rt = rt();
    let c = Arc::new(open(&rt, &mongo8()).unwrap());
    let mut sh = Shell::new(&rt, c, "rr_e2e_timeout", Duration::from_secs(1));
    sh.ok("db.slow.drop(); db.slow.insertOne({ a: 1 })");
    let started = Instant::now();
    let e = sh.error("db.slow.aggregate([{ $match: { $expr: { $function: { body: 'function() { var d = Date.now(); while (Date.now() - d < 3000) {} return true; }', args: [], lang: 'js' } } } }])");
    assert!(e.contains("timed out") || e.contains("JavaScript"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(3), "the shell gave up after its timeout");
    sh.ok("db.dropDatabase()");
}

#[test]
fn connection_errors_are_reported() {
    if !live() {
        return;
    }
    let rt = rt();
    // Wrong password: reported on the authentication step.
    let mut s = mongo8();
    s.credentials.password = "wrong".into();
    let mut steps = Vec::new();
    let err = rt.block_on(Connection::open(&s, &ConnectSecrets::default(), |st| steps.push(st))).err().unwrap();
    let msg = rustyrobo::mongo::error_message(&err);
    assert!(msg.contains("Authentication failed"), "{msg}");
    assert!(steps.iter().any(|s| s.kind == StepKind::Auth && !s.ok), "{steps:?}");

    // Password typed at connect time overrides the stored one.
    let secrets = ConnectSecrets { mongo_password: Some("secret".into()), ..Default::default() };
    assert!(rt.block_on(Connection::open(&s, &secrets, |_| {})).is_ok());

    // Nothing listening: a quick, readable error.
    let mut s = mongo42();
    s.hosts[0].port = 1;
    s.connect_timeout_secs = 2;
    let started = Instant::now();
    let err = open(&rt, &s).err().unwrap();
    assert!(started.elapsed() < Duration::from_secs(6));
    assert!(rustyrobo::mongo::error_message(&err).to_lowercase().contains("refused") || rustyrobo::mongo::error_message(&err).contains("timeout"));

    // Visible databases bypass listDatabases.
    let mut s = mongo8();
    s.use_visible_databases = true;
    s.visible_databases = vec!["only_this".into()];
    assert_eq!(open(&rt, &s).unwrap().databases, vec!["only_this"]);
}

#[test]
fn uri_built_connection_works() {
    if !live() {
        return;
    }
    let rt = rt();
    let mut s = ConnectionSettings::default();
    rustyrobo::mongo::uri::apply_uri(&mut s, "mongodb://admin:secret@127.0.0.1:27117/?authSource=admin&appName=e2e").unwrap();
    let c = open(&rt, &s).unwrap();
    assert!(c.databases.contains(&"admin".to_string()));
}

#[test]
fn ssh_tunnel_with_password() {
    if !live() {
        return;
    }
    let rt = rt();
    let mut steps = Vec::new();
    let c = rt
        .block_on(Connection::open(&via_ssh(SshAuthMethod::Password), &ConnectSecrets::default(), |st| steps.push(st)))
        .unwrap();
    assert!(c.tunnel.is_some());
    assert!(!c.databases.is_empty());
    assert!(steps.iter().any(|s| s.kind == StepKind::Ssh && s.ok));
    // The shell works through the tunnel.
    let mut sh = Shell::new(&rt, Arc::new(c), "rr_e2e_ssh", Duration::from_secs(20));
    assert_eq!(sh.text("db.t.insertOne({ a: 1 }); db.t.countDocuments({})"), "1");
    sh.ok("db.dropDatabase()");
}

#[test]
fn ssh_tunnel_with_private_keys() {
    if !live() {
        return;
    }
    let rt = rt();
    // Plain key.
    let mut s = via_ssh(SshAuthMethod::PrivateKey);
    s.ssh.private_key_file = key("test_key_plain");
    assert!(open(&rt, &s).unwrap().tunnel.is_some());

    // Encrypted key: missing passphrase is a clear error…
    s.ssh.private_key_file = key("test_key_encrypted");
    let err = open(&rt, &s).err().unwrap();
    assert!(format!("{err:#}").contains("passphrase"), "{err:#}");
    // …a stored passphrase works…
    s.ssh.passphrase = "testpass".into();
    assert!(open(&rt, &s).is_ok());
    // …and so does one typed at connect time.
    s.ssh.passphrase.clear();
    let secrets = ConnectSecrets {
        ssh: rustyrobo::mongo::ssh::SshSecrets { passphrase: Some("testpass".into()), ..Default::default() },
        ..Default::default()
    };
    assert!(rt.block_on(Connection::open(&s, &secrets, |_| {})).is_ok());
}

#[test]
fn ssh_failures_are_reported() {
    if !live() {
        return;
    }
    let rt = rt();
    let mut s = via_ssh(SshAuthMethod::Password);
    s.ssh.password = "wrong".into();
    let mut steps = Vec::new();
    let err = rt.block_on(Connection::open(&s, &ConnectSecrets::default(), |st| steps.push(st))).err().unwrap();
    assert!(format!("{err:#}").contains("SSH authentication failed"), "{err:#}");
    assert!(steps.iter().any(|s| s.kind == StepKind::Ssh && !s.ok));

    // Strict host key checking rejects a server we have never seen.
    let mut s = via_ssh(SshAuthMethod::Password);
    s.ssh.strict_host_key = true;
    // Fresh config dir: no recorded key.
    let dir = std::env::temp_dir().join(format!("rustyrobo-live-strict-{}", std::process::id()));
    let previous = std::env::var_os("RUSTYROBO_CONFIG_DIR");
    // SAFETY: tests run with --test-threads=1.
    unsafe { std::env::set_var("RUSTYROBO_CONFIG_DIR", &dir) };
    let err = open(&rt, &s).err().unwrap();
    if let Some(p) = previous {
        unsafe { std::env::set_var("RUSTYROBO_CONFIG_DIR", p) };
    }
    // ~/.ssh/known_hosts may know this host on a developer machine.
    let msg = format!("{err:#}");
    assert!(msg.contains("strict checking") || msg.contains("HOST KEY"), "{msg}");

    // Unreachable SSH server.
    let mut s = via_ssh(SshAuthMethod::Password);
    s.ssh.port = 1;
    assert!(format!("{:#}", open(&rt, &s).err().unwrap()).contains("SSH"));
}

/// Timing of large-document queries. Run with RR_BENCH=1 (and RR_LIVE=1):
///   RR_LIVE=1 RR_BENCH=1 cargo test --release --test live bench -- --nocapture
#[test]
fn bench_large_documents() {
    if !live() || std::env::var("RR_BENCH").is_err() {
        return;
    }
    let rt = rt();
    let c = Arc::new(open(&rt, &mongo8()).unwrap());
    let mut sh = Shell::new(&rt, c.clone(), "rr_bench", Duration::from_secs(120));
    // ~250 KB per document: nested steps with text and coordinates, like a roadbook.
    sh.ok(r#"db.dropDatabase();
        var steps = [];
        for (var s = 0; s < 400; s++) steps.push({ n: s, title: 'Step ' + s, note: 'x'.repeat(300), lat: 45.1 + s / 1000, lon: 5.7, tags: ['a', 'b'], at: new Date() });
        for (var i = 0; i < 120; i++) db.big.insertOne({ name: 'Roadbook ' + i, steps: steps, meta: { owner: 'bench', i: NumberLong(i) } })"#);
    let time = |sh: &mut Shell, script: &str| {
        let t = Instant::now();
        let outs = sh.run(script);
        let n = match &outs.last().unwrap().kind {
            OutputKind::Docs { docs, .. } => docs.len(),
            other => panic!("{other:?}"),
        };
        (t.elapsed(), n)
    };
    // Old path: the cursor is consumed by JavaScript (EJSON round trips).
    let (js_find, n1) = time(&mut sh, "var q = db.getCollection('big').find({}); q.hasNext(); q");
    let (js_agg, n2) = time(&mut sh, "var a = db.big.aggregate([{ $match: {} }]); a.hasNext(); a");
    // New path: documents fetched natively by Rust.
    let (native_find, n3) = time(&mut sh, "db.getCollection('big').find({})");
    let (native_agg, n4) = time(&mut sh, "db.big.aggregate([{ $match: {} }])");
    let t = Instant::now();
    let full = rt.block_on(rustyrobo::mongo::collect_cursor(&c, "rr_bench", doc! { "aggregate": "big", "pipeline": [{ "$sample": { "size": 100 } }], "cursor": {} }, 100, None)).unwrap();
    let full_sample = t.elapsed();
    let t = Instant::now();
    let keys = rt.block_on(rustyrobo::mongo::collect_cursor(&c, "rr_bench", rustyrobo::app::field_names_command("big"), 100, None)).unwrap();
    let keys_sample = t.elapsed();
    println!("find:      JS path {js_find:?} ({n1} docs) -> native {native_find:?} ({n3} docs)");
    println!("aggregate: JS path {js_agg:?} ({n2} docs) -> native {native_agg:?} ({n4} docs)");
    println!("field sampling: full documents {full_sample:?} ({}) -> keys only {keys_sample:?} ({})", full.len(), keys.len());
    assert!(native_find < js_find && native_agg < js_agg);
    sh.ok("db.dropDatabase()");
}
