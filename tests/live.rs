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
use rustyrobo::shell::{DropVars, OutputKind, ShellHandle, ShellOptions, ShellOutput, fetch_page};

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
        self.handle.execute(script.into(), DropVars::Only(vec![]));
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
    // A script declaring constants runs again (here keeping its variables),
    // and console.table draws its rows.
    let script = "const ages = new Set(db.people.distinct('age'))\n\
        const rows = db.people.aggregate([{ $group: { _id: null, n: { $sum: 1 } } }]).toArray()\n\
        rows.forEach(r => r.ages = ages.size)\n\
        console.table(rows, ['n', 'ages'])";
    let table = sh.text(script);
    assert!(table.starts_with('┌') && table.contains("│ n "), "{table}");
    assert_eq!(sh.text(script), table);
    // Dropped variables are gone at the next execution.
    sh.handle.execute("typeof ages".into(), DropVars::All);
    let outs = sh.rx.recv_timeout(Duration::from_secs(30)).expect("shell reply");
    assert!(outs.vars.is_empty(), "{:?}", outs.vars);
    assert!(matches!(&outs.outputs[0].kind, OutputKind::Text(t) if t == "undefined"), "{:?}", outs.outputs);

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

fn transfer_suite(rt: &tokio::runtime::Runtime, s: &ConnectionSettings, db: &str) {
    use rustyrobo::transfer::{Delimiter, ExportOptions, Format, ImportOptions, export, import};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let c = open(rt, s).unwrap();
    let dir = std::env::temp_dir().join(format!("rustyrobo-transfer-{db}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    rt.block_on(c.run_command(db, doc! { "dropDatabase": 1 }, None)).unwrap();
    let date = bson::DateTime::parse_rfc3339_str("2024-05-06T07:08:09.010Z").unwrap();
    let docs: Vec<bson::Document> = (0..2500)
        .map(|i| doc! { "_id": i, "name": format!("n{i}"), "big": 5_000_000_000i64 + i as i64, "at": date, "address": { "city": "Paris", "zip": "007" }, "tags": ["a", i] })
        .collect();
    for chunk in docs.chunks(1000) {
        rt.block_on(c.run_command(db, doc! { "insert": "src", "documents": chunk.to_vec() }, None)).unwrap();
    }
    let opts = |format: Format, file: &str| ExportOptions {
        db: db.into(),
        coll: "src".into(),
        filter: doc! {},
        projection: None,
        sort: Some(doc! { "_id": 1 }),
        skip: 0,
        limit: 0,
        format,
        canonical: false,
        fields: Vec::new(),
        delimiter: Delimiter::Comma,
        path: dir.join(file),
    };
    let into = |coll: &str, format: Format, file: &str| ImportOptions {
        db: db.into(),
        coll: coll.into(),
        format,
        delimiter: Delimiter::Comma,
        detect_types: true,
        drop_first: false,
        path: dir.join(file),
    };
    let all = |coll: &str| -> Vec<bson::Document> {
        rt.block_on(rustyrobo::mongo::collect_cursor(&c, db, doc! { "find": coll, "filter": {}, "sort": { "_id": 1 } }, 100_000, None)).unwrap()
    };
    let never = AtomicBool::new(false);

    // Every format round-trips, types included, in batches.
    for (format, file) in [(Format::JsonLines, "lines.json"), (Format::JsonArray, "array.json"), (Format::Csv, "docs.csv")] {
        let progress = AtomicU64::new(0);
        let n = rt.block_on(export(&c, &opts(format, file), &never, |n| progress.store(n, Ordering::Relaxed))).unwrap();
        assert_eq!((n, progress.load(Ordering::Relaxed)), (2500, 2500), "{file}");
        let coll = format!("back_{}", file.replace('.', "_"));
        let done = rt.block_on(import(&c, &into(&coll, format, file), Arc::new(AtomicBool::new(false)), |_| {})).unwrap();
        assert_eq!((done.inserted, done.failed), (2500, 0), "{file}");
        assert_eq!(all(&coll), docs, "{file}: same documents and types");
    }

    // A query: filter, projection, skip and limit.
    let mut q = opts(Format::Csv, "query.csv");
    q.filter = doc! { "_id": { "$gte": 100 } };
    q.projection = Some(doc! { "name": 1, "address.city": 1 });
    q.skip = 5;
    q.limit = 3;
    assert_eq!(rt.block_on(export(&c, &q, &never, |_| {})).unwrap(), 3);
    let text = std::fs::read_to_string(dir.join("query.csv")).unwrap();
    assert_eq!(text, "_id,name,address.city\r\n105,n105,Paris\r\n106,n106,Paris\r\n107,n107,Paris\r\n");

    // Duplicates are counted, the others inserted. Dropping first replaces
    // the collection only with all the documents of the file.
    std::fs::write(dir.join("dups.json"), "{\"_id\": 1}\n{\"_id\": 1}\n{\"_id\": 2}\n").unwrap();
    let done = rt.block_on(import(&c, &into("dups", Format::JsonLines, "dups.json"), Arc::new(AtomicBool::new(false)), |_| {})).unwrap();
    assert_eq!((done.inserted, done.failed), (2, 1));
    assert!(done.first_error.as_deref().unwrap_or_default().contains("E11000"), "{done:?}");
    let again = ImportOptions { drop_first: true, ..into("dups", Format::JsonLines, "dups.json") };
    let e = rt.block_on(import(&c, &again, Arc::new(AtomicBool::new(false)), |_| {})).unwrap_err().to_string();
    assert!(e.starts_with("1 document refused (E11000") && e.ends_with(": dups was left as it was"), "{e}");
    std::fs::write(dir.join("unique.json"), "{\"_id\": 3}\n{\"_id\": 4}\n").unwrap();
    let again = ImportOptions { drop_first: true, ..into("dups", Format::JsonLines, "unique.json") };
    assert_eq!(rt.block_on(import(&c, &again, Arc::new(AtomicBool::new(false)), |_| {})).unwrap().inserted, 2);
    assert_eq!(all("dups"), vec![doc! { "_id": 3 }, doc! { "_id": 4 }]);
    let fresh = ImportOptions { drop_first: true, ..into("never_existed", Format::JsonLines, "unique.json") };
    assert_eq!(rt.block_on(import(&c, &fresh, Arc::new(AtomicBool::new(false)), |_| {})).unwrap().inserted, 2, "nothing to drop");

    // Cancelled: stops between two batches.
    let cancel = AtomicBool::new(false);
    let e = rt.block_on(export(&c, &opts(Format::JsonLines, "cancel.json"), &cancel, |_| cancel.store(true, Ordering::Relaxed))).unwrap_err();
    assert!(e.to_string().starts_with("cancelled after 1000 documents"), "{e}");

    // Errors of the server and of the file.
    let mut bad = opts(Format::JsonLines, "bad.json");
    bad.filter = doc! { "$bad": 1 };
    assert!(rt.block_on(export(&c, &bad, &never, |_| {})).is_err());
    std::fs::write(dir.join("broken.json"), "{\"a\": 1}\n{\"a\":").unwrap();
    let e = rt.block_on(import(&c, &into("broken", Format::JsonLines, "broken.json"), Arc::new(AtomicBool::new(false)), |_| {})).unwrap_err();
    assert!(format!("{e:#}").contains("document 2"), "{e:#}");
    assert!(all("broken").is_empty(), "the file is read to the end before anything is written");
    let _ = std::fs::remove_dir_all(&dir);
}

fn dump_suite(rt: &tokio::runtime::Runtime, s: &ConnectionSettings, db: &str) {
    use rustyrobo::dump::{DumpOptions, RestoreOptions, dump, restore};
    use std::sync::atomic::{AtomicBool, Ordering};
    let c = open(rt, s).unwrap();
    let dir = std::env::temp_dir().join(format!("rustyrobo-dump-{db}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (copy, gz) = (format!("{db}_copy"), format!("{db}_gz"));
    for d in [db, copy.as_str(), gz.as_str()] {
        rt.block_on(c.run_command(d, doc! { "dropDatabase": 1 }, None)).unwrap();
    }
    let run = |d: &str, cmd: bson::Document| rt.block_on(c.run_command(d, cmd, None)).unwrap();
    let docs: Vec<bson::Document> = (0..2500).map(|i| doc! { "_id": i, "email": format!("u{i}@x"), "age": i % 90, "big": 5_000_000_000i64 + i as i64, "at": bson::DateTime::from_millis(i as i64) }).collect();
    for chunk in docs.chunks(1000) {
        run(db, doc! { "insert": "users", "documents": chunk.to_vec() });
    }
    run(db, doc! { "createIndexes": "users", "indexes": [{ "key": { "email": 1 }, "name": "email_1", "unique": true }] });
    run(db, doc! { "create": "logs", "capped": true, "size": 65536 });
    run(db, doc! { "insert": "logs", "documents": [{ "m": "hello" }] });
    run(db, doc! { "insert": "a/b c", "documents": [{ "_id": "slash" }] });
    run(db, doc! { "create": "empty" });
    run(db, doc! { "create": "adults", "viewOn": "users", "pipeline": [{ "$match": { "age": { "$gte": 18 } } }] });
    let all = |d: &str, coll: &str| -> Vec<bson::Document> {
        rt.block_on(rustyrobo::mongo::collect_cursor(&c, d, doc! { "find": coll, "filter": {}, "sort": { "_id": 1 } }, 100_000, None)).unwrap()
    };
    let names = |d: &str| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = rt
            .block_on(rustyrobo::mongo::collect_cursor(&c, d, doc! { "listCollections": 1, "cursor": {} }, 1000, None))
            .unwrap()
            .iter()
            .filter(|i| !i.get_str("name").unwrap().starts_with("system."))
            .map(|i| (i.get_str("name").unwrap().to_string(), i.get_str("type").unwrap_or("collection").to_string()))
            .collect();
        v.sort();
        v
    };
    let never = AtomicBool::new(false);

    // The files of mongodump.
    let opts = DumpOptions { db: db.into(), collections: Vec::new(), dir: dir.clone(), gzip: false, archive: false };
    let done = rt.block_on(dump(&c, &opts, &never, |_| {})).unwrap();
    assert_eq!((done.collections, done.documents), (5, 2502));
    let folder = dir.join(db);
    assert_eq!(opts.target(), folder);
    for f in ["users.bson", "users.metadata.json", "a%2Fb+c.bson", "logs.metadata.json", "empty.bson", "adults.metadata.json"] {
        assert!(folder.join(f).is_file(), "{f}");
    }
    assert!(!folder.join("adults.bson").exists(), "a view has no documents");
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(folder.join("users.metadata.json")).unwrap()).unwrap();
    assert_eq!(meta["collectionName"], "users");
    assert_eq!(meta["indexes"][1]["name"], "email_1");
    assert_eq!(meta["indexes"][1]["key"]["email"], serde_json::json!({ "$numberInt": "1" }), "canonical Extended JSON");

    // Restored elsewhere: documents, types, options, indexes and views.
    let into = |d: &str, from: std::path::PathBuf| RestoreOptions { db: d.into(), from, collections: Vec::new(), drop_first: false, indexes: true };
    let r = rt.block_on(restore(&c, &into(&copy, folder.clone()), Arc::new(AtomicBool::new(false)), |_| {})).unwrap();
    assert_eq!((r.collections, r.docs.inserted, r.docs.failed), (5, 2502, 0));
    assert_eq!(names(&copy), names(db));
    assert_eq!(all(&copy, "users"), docs);
    assert_eq!(all(&copy, "a/b c"), vec![doc! { "_id": "slash" }]);
    assert_eq!(all(&copy, "adults").len(), all(db, "adults").len());
    let stats = run(&copy, doc! { "collStats": "logs" });
    assert_eq!(stats.get_bool("capped").ok(), Some(true));
    let indexes = rt.block_on(rustyrobo::mongo::collect_cursor(&c, &copy, doc! { "listIndexes": "users", "cursor": {} }, 100, None)).unwrap();
    let email = indexes.iter().find(|i| i.get_str("name").ok() == Some("email_1")).expect("email index");
    assert_eq!(email.get_bool("unique").ok(), Some(true));

    // Again: the duplicates are counted; dropping first starts again.
    let r = rt.block_on(restore(&c, &into(&copy, folder.clone()), Arc::new(AtomicBool::new(false)), |_| {})).unwrap();
    assert_eq!((r.docs.inserted, r.docs.failed), (0, 2502));
    assert!(r.docs.first_error.unwrap_or_default().contains("E11000"));
    let again = RestoreOptions { drop_first: true, ..into(&copy, folder.clone()) };
    assert_eq!(rt.block_on(restore(&c, &again, Arc::new(AtomicBool::new(false)), |_| {})).unwrap().docs.inserted, 2502);

    // Gzipped, and some of the collections.
    let opts = DumpOptions { db: db.into(), collections: vec!["users".into(), "adults".into()], dir: dir.join("gz"), gzip: true, archive: false };
    std::fs::create_dir_all(&opts.dir).unwrap();
    assert_eq!(rt.block_on(dump(&c, &opts, &never, |_| {})).unwrap().collections, 2);
    assert!(opts.target().join("users.bson.gz").is_file() && opts.target().join("users.metadata.json.gz").is_file());
    let r = rt.block_on(restore(&c, &into(&gz, opts.target()), Arc::new(AtomicBool::new(false)), |_| {})).unwrap();
    assert_eq!((r.collections, r.docs.inserted), (2, 2500));
    assert_eq!(all(&gz, "users"), docs);
    assert_eq!(names(&gz), vec![("adults".into(), "view".into()), ("users".into(), "collection".into())]);

    // An archive, gzipped or not: one file, read back with its options, indexes and views.
    for gzip in [false, true] {
        let opts = DumpOptions { db: db.into(), collections: Vec::new(), dir: dir.clone(), gzip, archive: true };
        let done = rt.block_on(dump(&c, &opts, &never, |_| {})).unwrap();
        assert_eq!((done.collections, done.documents), (5, 2502));
        assert!(opts.target().is_file() && opts.target().ends_with(if gzip { format!("{db}.archive.gz") } else { format!("{db}.archive") }));
        let found = rustyrobo::dump::scan(&opts.target(), db).unwrap();
        assert_eq!(found.iter().filter(|e| e.view).map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["adults"]);
        let again = RestoreOptions { drop_first: true, ..into(&copy, opts.target()) };
        let r = rt.block_on(restore(&c, &again, Arc::new(AtomicBool::new(false)), |_| {})).unwrap();
        assert_eq!((r.collections, r.docs.inserted, r.docs.failed), (5, 2502, 0), "gzip: {gzip}");
        assert_eq!(names(&copy), names(db));
        assert_eq!(all(&copy, "users"), docs);
        assert_eq!(all(&copy, "adults").len(), all(db, "adults").len());
        assert_eq!(run(&copy, doc! { "collStats": "logs" }).get_bool("capped").ok(), Some(true));
    }
    let only = RestoreOptions { collections: vec!["logs".into()], drop_first: true, ..into(&gz, dir.join(format!("{db}.archive"))) };
    let r = rt.block_on(restore(&c, &only, Arc::new(AtomicBool::new(false)), |_| {})).unwrap();
    assert_eq!((r.collections, r.docs.inserted), (1, 1), "some collections of an archive");

    // Cancelled between two batches.
    let cancel = AtomicBool::new(false);
    let opts = DumpOptions { db: db.into(), collections: vec!["users".into()], dir: dir.join("cancel"), gzip: false, archive: false };
    std::fs::create_dir_all(&opts.dir).unwrap();
    let e = rt.block_on(dump(&c, &opts, &cancel, |_| cancel.store(true, Ordering::Relaxed))).unwrap_err();
    assert!(format!("{e:#}").contains("cancelled after 1000 documents"), "{e:#}");
    for d in [db, copy.as_str(), gz.as_str()] {
        run(d, doc! { "dropDatabase": 1 });
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dump_and_restore_on_a_modern_server() {
    if !live() {
        return;
    }
    dump_suite(&rt(), &mongo8(), "rr_e2e_dump_modern");
}

#[test]
fn dump_and_restore_on_a_legacy_server() {
    if !live() {
        return;
    }
    dump_suite(&rt(), &mongo42(), "rr_e2e_dump_legacy");
}

#[test]
fn import_and_export_on_a_modern_server() {
    if !live() {
        return;
    }
    transfer_suite(&rt(), &mongo8(), "rr_e2e_transfer_modern");
}

#[test]
fn import_and_export_on_a_legacy_server() {
    if !live() {
        return;
    }
    transfer_suite(&rt(), &mongo42(), "rr_e2e_transfer_legacy");
}

// ------------------------------------------------ what imports and restores keep

/// A server of the test environment, with the container that holds the
/// MongoDB tools of its version.
struct Tools {
    settings: ConnectionSettings,
    container: &'static str,
    /// Options of the tools, run in the container.
    auth: &'static str,
    name: &'static str,
}

fn modern_tools() -> Tools {
    Tools { settings: mongo8(), container: "rr-mongo8", auth: "-u admin -p secret --authenticationDatabase admin", name: "modern" }
}

fn legacy_tools() -> Tools {
    Tools { settings: mongo42(), container: "rr-mongo42", auth: "", name: "legacy" }
}

impl Tools {
    /// Runs a shell script in the container.
    fn sh(&self, script: &str) -> String {
        let out = std::process::Command::new("docker").args(["exec", self.container, "sh", "-c", script]).output().expect("docker");
        assert!(out.status.success(), "{script}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Runs mongodump, mongorestore, mongoexport or mongoimport.
    fn tool(&self, name: &str, args: &str) -> String {
        self.sh(&format!("{name} {} --quiet {args}", self.auth))
    }

    fn copy_in(&self, host: &std::path::Path, inside: &str) {
        self.sh(&format!("rm -rf {inside} && mkdir -p $(dirname {inside})"));
        docker_cp(&host.display().to_string(), &format!("{}:{inside}", self.container));
    }

    fn copy_out(&self, inside: &str, host: &std::path::Path) {
        let _ = std::fs::remove_dir_all(host);
        let _ = std::fs::remove_file(host);
        docker_cp(&format!("{}:{inside}", self.container), &host.display().to_string());
    }
}

fn docker_cp(from: &str, to: &str) {
    let out = std::process::Command::new("docker").args(["cp", from, to]).output().expect("docker");
    assert!(out.status.success(), "docker cp {from} {to}: {}", String::from_utf8_lossy(&out.stderr));
}

fn run(rt: &tokio::runtime::Runtime, c: &Connection, db: &str, cmd: bson::Document) -> bson::Document {
    let what = format!("{cmd:?}");
    rt.block_on(c.run_command(db, cmd, None)).unwrap_or_else(|e| panic!("{}: {}", what.chars().take(200).collect::<String>(), rustyrobo::mongo::error_message(&e)))
}

fn insert(rt: &tokio::runtime::Runtime, c: &Connection, db: &str, coll: &str, docs: &[bson::Document]) {
    for chunk in docs.chunks(500) {
        let reply = run(rt, c, db, doc! { "insert": coll, "documents": chunk.to_vec() });
        assert!(reply.get("writeErrors").is_none(), "{coll}: {reply:?}");
    }
}

fn find_all(rt: &tokio::runtime::Runtime, c: &Connection, db: &str, coll: &str) -> Vec<bson::Document> {
    rt.block_on(rustyrobo::mongo::collect_cursor(c, db, doc! { "find": coll, "filter": {}, "sort": { "_id": 1 } }, 1_000_000, None)).unwrap()
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rustyrobo-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether the server is at least `major.minor`.
fn at_least(c: &Connection, major: u32, minor: u32) -> bool {
    let mut parts = c.version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= (major, minor)
}

/// A collection as an import or a restore may change it: type, options,
/// indexes, and documents byte for byte (types, the sign of zero and NaN
/// included).
#[derive(Debug, PartialEq, Clone)]
struct Kept {
    kind: String,
    options: bson::Document,
    indexes: Vec<bson::Document>,
    docs: Vec<Vec<u8>>,
}

/// Everything of a database an import or a restore may change, by
/// collection; the definitions of views and the buckets of time series
/// show in their collection.
type Snapshot = std::collections::BTreeMap<String, Kept>;

fn snapshot(rt: &tokio::runtime::Runtime, c: &Connection, db: &str) -> Snapshot {
    let infos = rt.block_on(rustyrobo::mongo::collect_cursor(c, db, doc! { "listCollections": 1, "cursor": {} }, 10_000, None)).unwrap();
    let mut out = Snapshot::new();
    for info in infos {
        let name = info.get_str("name").unwrap().to_string();
        if name == "system.views" || name.starts_with("system.buckets.") {
            continue;
        }
        let kind = info.get_str("type").unwrap_or("collection").to_string();
        let (indexes, docs) = if kind == "view" {
            (Vec::new(), Vec::new())
        } else {
            let mut indexes = rt.block_on(rustyrobo::mongo::collect_cursor(c, db, doc! { "listIndexes": &name, "cursor": {} }, 1000, None)).unwrap();
            for i in &mut indexes {
                i.remove("v");
                i.remove("ns");
            }
            indexes.sort_by(|a, b| a.get_str("name").unwrap().cmp(b.get_str("name").unwrap()));
            (indexes, find_all(rt, c, db, &name).iter().map(|d| bson::to_vec(d).unwrap()).collect())
        };
        out.insert(name, Kept { kind, options: info.get_document("options").cloned().unwrap_or_default(), indexes, docs });
    }
    out
}

/// Byte for byte, with the name of the first field that differs.
fn assert_same_docs(got: &[bson::Document], expected: &[bson::Document], what: &str) {
    assert_eq!(got.len(), expected.len(), "{what}: number of documents");
    for (g, e) in got.iter().zip(expected) {
        if bson::to_vec(g).unwrap() == bson::to_vec(e).unwrap() {
            continue;
        }
        for (k, v) in e {
            let raw = |b: Option<&Bson>| b.map(|b| bson::to_vec(&doc! { "v": b.clone() }).unwrap());
            assert_eq!(raw(g.get(k)), raw(Some(v)), "{what}: {k}: {:?} instead of {v:?}", g.get(k));
        }
        panic!("{what}: other fields or order: {g:?}");
    }
}

fn assert_same_db(got: &Snapshot, expected: &Snapshot, what: &str) {
    assert_eq!(got.keys().collect::<Vec<_>>(), expected.keys().collect::<Vec<_>>(), "{what}: collections");
    for (name, e) in expected {
        let g = &got[name];
        assert_eq!((&g.kind, &g.options, &g.indexes), (&e.kind, &e.options, &e.indexes), "{what}: {name}");
        assert!(g.docs == e.docs, "{what}: documents of {name} differ ({} instead of {})", g.docs.len(), e.docs.len());
    }
}

/// Documents with a value of each type BSON has, and the edge values that
/// conversions lose most.
fn every_type() -> Vec<bson::Document> {
    use bson::spec::BinarySubtype;
    use bson::{Binary, DateTime, Decimal128, Timestamp, oid::ObjectId};
    let bin = |subtype: u8, bytes: &[u8]| Bson::Binary(Binary { subtype: BinarySubtype::from(subtype), bytes: bytes.to_vec() });
    let dec = |s: &str| Bson::Decimal128(s.parse::<Decimal128>().unwrap());
    let date = DateTime::from_millis;
    let pointer = Bson::try_from(serde_json::json!({ "$dbPointer": { "$ref": "c", "$id": { "$oid": "5f1d7a0e1c9d440000a1b2c4" } } })).unwrap();
    let mut deep = doc! { "bottom": 1 };
    for _ in 0..90 {
        deep = doc! { "d": deep };
    }
    let mut d = doc! {
        "_id": ObjectId::parse_str("5f1d7a0e1c9d440000a1b2c3").unwrap(),
        "double": 1.5, "whole": 2.0, "negZero": -0.0, "nan": f64::NAN, "inf": f64::INFINITY, "ninf": f64::NEG_INFINITY,
        "tiny": 5e-324, "huge": f64::MAX, "third": 1.0 / 3.0, "big": 1e300,
        "int": 7, "intMin": i32::MIN, "intMax": i32::MAX,
        "long": 9_007_199_254_740_993i64, "smallLong": 5i64, "longMin": i64::MIN, "longMax": i64::MAX,
    };
    d.extend(doc! {
        "dec": dec("123.456"), "decNaN": dec("NaN"), "decInf": dec("-Infinity"), "decTiny": dec("1E-6176"), "decNegZero": dec("-0"), "decLong": dec("1234567890123456789012345678901234"),
        "string": "héllo \0 nul \n \"q\" \\ \u{1F600} שלום مرحبا 你好", "empty": "",
        "date": date(1_706_933_106_789), "before1970": date(-1), "year1": date(-62_135_596_800_000), "after9999": date(253_402_300_800_000), "dateMin": DateTime::MIN, "dateMax": DateTime::MAX,
    });
    d.extend(doc! {
        "binary": bin(0, &[1, 2, 3]), "function": bin(1, &[1]), "old": bin(2, &[1, 2]), "legacyUuid": bin(3, &[7; 16]), "uuid": bin(4, &[8; 16]),
        "md5": bin(5, &[9; 16]), "encrypted": bin(6, &[1]), "sensitive": bin(8, &[1]), "user": bin(0x80, &[0xff]), "emptyBinary": bin(0, &[]),
        "regex": Bson::RegularExpression(bson::Regex { pattern: "ab+c".into(), options: "im".into() }), "emptyRegex": Bson::RegularExpression(bson::Regex { pattern: String::new(), options: String::new() }),
        "ts": Timestamp { time: 10, increment: 2 }, "tsMax": Timestamp { time: u32::MAX, increment: u32::MAX },
    });
    d.extend(doc! {
        "min": Bson::MinKey, "max": Bson::MaxKey, "null": Bson::Null, "undefined": Bson::Undefined, "yes": true, "no": false,
        "symbol": Bson::Symbol("sym".into()), "code": Bson::JavaScriptCode("function () { return 1 }".into()),
        "codeScope": Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope { code: "function () { return x }".into(), scope: doc! { "x": 1i64 } }),
        "pointer": pointer,
        "array": [1, "a", { "x": true }, [], [[1i64]]], "emptyArray": [], "emptyDoc": {},
        "keys": { "": 1, "a.b": 2, "$dollar": 3, "ünï": 4, "with space": 5, "_id": 6 },
        "deep": deep,
    });
    let mut other = d.clone();
    other.insert("_id", "second");
    other.insert("long", -1i64);
    // In the order of their _id, a string before an ObjectId.
    vec![other, d]
}

/// `v` as relaxed Extended JSON gives it back: a long that fits in 32 bits
/// comes back as an int, with mongoimport too.
fn relaxed(v: &Bson) -> Bson {
    match v {
        Bson::Int64(n) if i32::try_from(*n).is_ok() => Bson::Int32(*n as i32),
        Bson::Document(d) => Bson::Document(d.iter().map(|(k, v)| (k.clone(), relaxed(v))).collect()),
        Bson::Array(a) => Bson::Array(a.iter().map(relaxed).collect()),
        Bson::JavaScriptCodeWithScope(c) => {
            let Bson::Document(scope) = relaxed(&Bson::Document(c.scope.clone())) else { unreachable!() };
            Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope { code: c.code.clone(), scope })
        }
        other => other.clone(),
    }
}

/// Extended JSON has one NaN: mongoimport reads it as the NaN of Go, whose
/// last bit is set.
fn one_nan(v: &Bson) -> Bson {
    match v {
        Bson::Double(f) if f.is_nan() => Bson::Double(f64::NAN),
        Bson::Document(d) => Bson::Document(d.iter().map(|(k, v)| (k.clone(), one_nan(v))).collect()),
        Bson::Array(a) => Bson::Array(a.iter().map(one_nan).collect()),
        other => other.clone(),
    }
}

fn one_nan_docs(docs: &[bson::Document]) -> Vec<bson::Document> {
    docs.iter().map(|d| if let Bson::Document(d) = one_nan(&Bson::Document(d.clone())) { d } else { unreachable!() }).collect()
}

fn relaxed_docs(docs: &[bson::Document]) -> Vec<bson::Document> {
    docs.iter().map(|d| if let Bson::Document(d) = relaxed(&Bson::Document(d.clone())) { d } else { unreachable!() }).collect()
}

fn import_options(db: &str, coll: &str, path: std::path::PathBuf, drop_first: bool) -> rustyrobo::transfer::ImportOptions {
    rustyrobo::transfer::ImportOptions {
        db: db.into(),
        coll: coll.into(),
        format: rustyrobo::transfer::Format::of_path(&path),
        delimiter: rustyrobo::transfer::Delimiter::of_path(&path),
        detect_types: true,
        drop_first,
        path,
    }
}

fn export_options(db: &str, coll: &str, format: rustyrobo::transfer::Format, canonical: bool, path: std::path::PathBuf) -> rustyrobo::transfer::ExportOptions {
    rustyrobo::transfer::ExportOptions {
        db: db.into(),
        coll: coll.into(),
        filter: doc! {},
        projection: None,
        sort: Some(doc! { "_id": 1 }),
        skip: 0,
        limit: 0,
        format,
        canonical,
        fields: Vec::new(),
        delimiter: rustyrobo::transfer::Delimiter::Comma,
        path,
    }
}

fn restore_options(db: &str, from: std::path::PathBuf, drop_first: bool) -> rustyrobo::dump::RestoreOptions {
    rustyrobo::dump::RestoreOptions { db: db.into(), from, collections: Vec::new(), drop_first, indexes: true }
}

fn never() -> Arc<std::sync::atomic::AtomicBool> {
    Arc::new(std::sync::atomic::AtomicBool::new(false))
}

/// A database with what dumps and restores must keep: every type, a lot of
/// documents, indexes of each kind, options, views, stored functions, odd
/// names; a time series and a clustered collection on modern servers.
fn seed(rt: &tokio::runtime::Runtime, c: &Connection, db: &str) {
    run(rt, c, db, doc! { "dropDatabase": 1 });
    insert(rt, c, db, "types", &every_type());
    let many: Vec<bson::Document> = (0..2500)
        // Dates far ahead: the TTL monitor deletes nothing during the tests.
        .map(|i| doc! { "_id": i, "email": format!("u{i}@x"), "age": i % 90, "name": format!("n{i}"), "at": bson::DateTime::from_millis(4_000_000_000_000 + i as i64 * 1000), "text": "lorem ipsum" })
        .collect();
    insert(rt, c, db, "many", &many);
    run(rt, c, db, doc! { "createIndexes": "many", "indexes": [
        { "key": { "email": 1 }, "name": "email_1", "unique": true },
        { "key": { "age": -1, "name": 1 }, "name": "age_name" },
        { "key": { "name": 1 }, "name": "partial", "partialFilterExpression": { "age": { "$gt": 10 } } },
        { "key": { "at": 1 }, "name": "ttl", "expireAfterSeconds": 31_536_000 },
        { "key": { "text": "text" }, "name": "text" },
        { "key": { "age": 1 }, "name": "sparse", "sparse": true },
    ] });
    run(rt, c, db, doc! { "create": "capped", "capped": true, "size": 65536, "max": 100 });
    insert(rt, c, db, "capped", &(0..10).map(|i| doc! { "_id": i }).collect::<Vec<_>>());
    // Documents its validator came after, which a restore must keep.
    insert(rt, c, db, "valid", &[doc! { "_id": 1, "n": 1 }, doc! { "_id": 2, "n": "not a number" }]);
    run(rt, c, db, doc! { "collMod": "valid", "validator": { "n": { "$type": "number" } }, "validationLevel": "moderate" });
    run(rt, c, db, doc! { "create": "collated", "collation": { "locale": "fr", "strength": 2 } });
    insert(rt, c, db, "collated", &[doc! { "_id": "é" }]);
    run(rt, c, db, doc! { "create": "adults", "viewOn": "many", "pipeline": [{ "$match": { "age": { "$gte": 18 } } }] });
    insert(rt, c, db, "system.js", &[doc! { "_id": "double", "value": Bson::JavaScriptCode("function (x) { return 2 * x }".into()) }]);
    insert(rt, c, db, "x+y", &[doc! { "_id": "plus" }]);
    insert(rt, c, db, "a/b c", &[doc! { "_id": "slash" }]);
    run(rt, c, db, doc! { "create": "empty" });
    if !c.is_legacy() {
        run(rt, c, db, doc! { "create": "ts", "timeseries": { "timeField": "t", "metaField": "m" } });
        let measurements: Vec<bson::Document> = (0..300).map(|i| doc! { "_id": i, "t": bson::DateTime::from_millis(1_700_000_000_000 + i * 60_000), "m": format!("s{}", i % 3), "v": i as f64 / 3.0 }).collect();
        insert(rt, c, db, "ts", &measurements);
        if at_least(c, 5, 3) {
            run(rt, c, db, doc! { "create": "clustered", "clusteredIndex": { "key": { "_id": 1 }, "unique": true } });
            insert(rt, c, db, "clustered", &[doc! { "_id": 1 }, doc! { "_id": 2 }]);
        }
    }
}

/// Exports and imports, dumps and restores, of rustyrobo and of the MongoDB
/// tools, between them: the documents come back byte for byte, with the
/// options, indexes and views of their collections.
fn fidelity_suite(rt: &tokio::runtime::Runtime, t: &Tools) {
    use rustyrobo::dump::{DumpOptions, dump, restore};
    use rustyrobo::transfer::{Format, export, import};
    let c = open(rt, &t.settings).unwrap();
    let db = format!("rr_e2e_fidelity_{}", t.name);
    let copy = format!("{db}_copy");
    let dir = temp(&format!("fidelity-{}", t.name));
    let inside = format!("/tmp/rr-e2e-{}", t.name);
    seed(rt, &c, &db);
    let source = snapshot(rt, &c, &db);
    let types = find_all(rt, &c, &db, "types");
    assert_same_docs(&types, &every_type(), "inserted as built");

    // Our exports, read back by our import.
    for (format, canonical, file) in [(Format::JsonLines, true, "lines.json"), (Format::JsonArray, true, "array.json"), (Format::JsonLines, false, "relaxed.json"), (Format::JsonArray, false, "relaxed-array.json")] {
        let path = dir.join(file);
        assert_eq!(rt.block_on(export(&c, &export_options(&db, "types", format, canonical, path.clone()), &std::sync::atomic::AtomicBool::new(false), |_| {})).unwrap(), 2);
        let done = rt.block_on(import(&c, &import_options(&db, "back", path, true), never(), |_| {})).unwrap();
        assert_eq!((done.inserted, done.failed), (2, 0), "{file}");
        let expected = if canonical { types.clone() } else { relaxed_docs(&types) };
        assert_same_docs(&find_all(rt, &c, &db, "back"), &expected, file);
    }
    run(rt, &c, &db, doc! { "drop": "back" });

    // mongoexport, read by our import.
    t.sh(&format!("rm -rf {inside} && mkdir -p {inside}"));
    for (format, file) in [("canonical", "tool.json"), ("relaxed", "tool-relaxed.json")] {
        t.tool("mongoexport", &format!("-d {db} -c types --jsonFormat={format} --sort '{{_id: 1}}' -o {inside}/{file}"));
        t.copy_out(&format!("{inside}/{file}"), &dir.join(file));
        rt.block_on(import(&c, &import_options(&db, "back", dir.join(file), true), never(), |_| {})).unwrap();
        let expected = if format == "canonical" { types.clone() } else { relaxed_docs(&types) };
        assert_same_docs(&find_all(rt, &c, &db, "back"), &expected, file);
    }
    // Our export, read by mongoimport.
    for (format, canonical, file, options) in [(Format::JsonLines, true, "ours.json", ""), (Format::JsonArray, true, "ours-array.json", "--jsonArray"), (Format::JsonLines, false, "ours-relaxed.json", "")] {
        rt.block_on(export(&c, &export_options(&db, "types", format, canonical, dir.join(file)), &std::sync::atomic::AtomicBool::new(false), |_| {})).unwrap();
        t.copy_in(&dir.join(file), &format!("{inside}/{file}"));
        t.tool("mongoimport", &format!("-d {db} -c back --drop {options} --file {inside}/{file}"));
        let expected = if canonical { types.clone() } else { relaxed_docs(&types) };
        assert_same_docs(&one_nan_docs(&find_all(rt, &c, &db, "back")), &expected, &format!("mongoimport of {file}"));
    }
    run(rt, &c, &db, doc! { "drop": "back" });

    // Our dumps, restored by us, into an empty database and over one.
    for (gzip, archive) in [(false, false), (true, false), (false, true), (true, true)] {
        let what = format!("gzip {gzip}, archive {archive}");
        let opts = DumpOptions { db: db.clone(), collections: Vec::new(), dir: dir.join("ours"), gzip, archive };
        std::fs::create_dir_all(&opts.dir).unwrap();
        let _ = std::fs::remove_dir_all(opts.target());
        rt.block_on(dump(&c, &opts, &std::sync::atomic::AtomicBool::new(false), |_| {})).unwrap();
        run(rt, &c, &copy, doc! { "dropDatabase": 1 });
        for drop_first in [false, true] {
            let r = rt.block_on(restore(&c, &restore_options(&copy, opts.target(), drop_first), never(), |_| {})).unwrap();
            assert!(r.problems.is_empty() && r.docs.failed == 0, "{what}, drop {drop_first}: {}", r.summary());
            assert_same_db(&snapshot(rt, &c, &copy), &source, &format!("{what}, drop {drop_first}"));
        }
        // Read by mongorestore.
        let name = if archive { "in.archive".to_string() } else { "in".to_string() };
        t.copy_in(&opts.target(), &format!("{inside}/{name}"));
        let from = if archive { format!("--archive={inside}/{name}") } else { format!("--dir={inside}/{name}") };
        let ns = if archive { format!("--nsFrom='{db}.*' --nsTo='{copy}.*'") } else { format!("-d {copy}") };
        let gz = if gzip { "--gzip" } else { "" };
        t.tool("mongorestore", &format!("--drop --bypassDocumentValidation {gz} {ns} {from}"));
        let mut restored = snapshot(rt, &c, &copy);
        if c.is_legacy() && !archive {
            // mongorestore 4.2 reads the "+" of mongodump 100 (a space) as a "+".
            let slash = restored.remove("a/b+c").expect("a/b c as mongorestore 4.2 names it");
            restored.insert("a/b c".into(), slash);
        }
        assert_same_db(&restored, &source, &format!("mongorestore of our dump, {what}"));
    }

    // The dumps of mongodump, restored by us.
    for (gzip, archive) in [(false, false), (true, false), (false, true), (true, true)] {
        let what = format!("mongodump, gzip {gzip}, archive {archive}");
        let gz = if gzip { "--gzip" } else { "" };
        let (target, from) = if archive {
            t.tool("mongodump", &format!("-d {db} {gz} --archive={inside}/dump.archive"));
            (format!("{inside}/dump.archive"), dir.join("dump.archive"))
        } else {
            t.sh(&format!("rm -rf {inside}/dump"));
            t.tool("mongodump", &format!("-d {db} {gz} -o {inside}/dump"));
            (format!("{inside}/dump/{db}"), dir.join("dump"))
        };
        t.copy_out(&target, &from);
        run(rt, &c, &copy, doc! { "dropDatabase": 1 });
        let r = rt.block_on(restore(&c, &restore_options(&copy, from.clone(), false), never(), |_| {})).unwrap();
        assert!(r.problems.is_empty() && r.docs.failed == 0, "{what}: {}", r.summary());
        assert_same_db(&snapshot(rt, &c, &copy), &source, &what);
        let r = rt.block_on(restore(&c, &restore_options(&copy, from, true), never(), |_| {})).unwrap();
        assert!(r.problems.is_empty(), "{what}, again: {}", r.summary());
        assert_same_db(&snapshot(rt, &c, &copy), &source, &format!("{what}, over itself"));
    }
    for d in [&db, &copy] {
        run(rt, &c, d, doc! { "dropDatabase": 1 });
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn everything_comes_back_on_a_modern_server() {
    if !live() {
        return;
    }
    fidelity_suite(&rt(), &modern_tools());
}

#[test]
fn everything_comes_back_on_a_legacy_server() {
    if !live() {
        return;
    }
    fidelity_suite(&rt(), &legacy_tools());
}

/// An import that does not complete leaves the collection as it was: a
/// file it cannot read changes nothing, "drop first" replaces only with
/// all the documents of the file.
fn import_safety_suite(rt: &tokio::runtime::Runtime, t: &Tools) {
    use rustyrobo::transfer::{Progress, import};
    use std::sync::atomic::Ordering;
    let c = open(rt, &t.settings).unwrap();
    let db = format!("rr_e2e_import_safety_{}", t.name);
    let dir = temp(&format!("import-safety-{}", t.name));
    run(rt, &c, &db, doc! { "dropDatabase": 1 });
    let precious: Vec<bson::Document> = (0..50).map(|i| doc! { "_id": i, "email": format!("u{i}@x"), "n": i }).collect();
    insert(rt, &c, &db, "precious", &precious);
    run(rt, &c, &db, doc! { "createIndexes": "precious", "indexes": [{ "key": { "email": 1 }, "name": "email_1", "unique": true }] });
    run(rt, &c, &db, doc! { "collMod": "precious", "validator": { "n": { "$type": "number" } } });
    run(rt, &c, &db, doc! { "create": "precious_view", "viewOn": "precious", "pipeline": [] });
    let before = snapshot(rt, &c, &db);
    let file = |name: &str, content: Vec<u8>| {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    };
    let lines = |n: usize| -> String { (0..n).map(|i| format!("{{\"_id\":{},\"email\":\"l{i}@x\",\"n\":{i}}}\n", 1000 + i)).collect() };
    let mut failing: Vec<(&str, std::path::PathBuf, &str)> = vec![
        ("broken at the first document", file("first.json", b"{\"a\":".to_vec()), "document 1"),
        ("broken after two batches", file("late.json", format!("{}{{\"a\":", lines(2500)).into_bytes()), "document 2501"),
        ("broken in an array", file("array.json", format!("[{}oops]", lines(1500).replace('\n', ",")).into_bytes()), "document 1501"),
        ("not UTF-8", file("latin1.csv", [b"n\n".to_vec(), "1\n".repeat(1500).into_bytes(), b"caf\xe9\n".to_vec()].concat()), "line 1502: the file is not UTF-8 text"),
        ("two columns for one field", file("conflict.csv", b"a,a.b\n1,2\n".to_vec()), "write the same field"),
        ("a number too big", file("big-number.json", format!("{}{{\"n\":18446744073709551615}}", lines(10)).into_bytes()), "does not fit in a 64-bit integer"),
        ("a document over 16 MB", file("too-big.json", format!("{}{{\"s\":\"{}\"}}\n", lines(1200), "x".repeat(16 * 1024 * 1024)).into_bytes()), "more than the 16 MB"),
    ];
    if c.is_legacy() {
        // A field name the server refuses.
        failing.push(("refused by the server", file("dollar.json", format!("{}{{\"_id\":1,\"$set\":1}}\n", lines(10)).into_bytes()), "refused"));
    }
    let dups = file("dups.json", format!("{}{{\"_id\":1000}}\n", lines(1500)).into_bytes());
    for (what, path, expected) in &failing {
        for drop_first in [true, false] {
            let e = rt.block_on(import(&c, &import_options(&db, "precious", path.clone(), drop_first), never(), |_| {}));
            if !drop_first && *what == "refused by the server" {
                // Added to the collection: counted.
                assert_eq!(e.unwrap().failed, 1);
                run(rt, &c, &db, doc! { "delete": "precious", "deletes": [{ "q": { "_id": { "$gte": 1000 } }, "limit": 0 }] });
                continue;
            }
            let e = format!("{:#}", e.unwrap_err());
            assert!(e.contains(expected), "{what}, drop {drop_first}: {e}");
            if *what == "refused by the server" {
                assert!(e.ends_with("precious was left as it was"), "{e}");
            }
            assert_same_db(&snapshot(rt, &c, &db), &before, &format!("{what}, drop {drop_first}"));
        }
    }
    // Duplicates in the file: counted when added, but no replacement by an incomplete copy.
    let e = rt.block_on(import(&c, &import_options(&db, "precious", dups.clone(), true), never(), |_| {})).unwrap_err().to_string();
    assert!(e.starts_with("1 document refused (E11000") && e.ends_with(": precious was left as it was"), "{e}");
    assert_same_db(&snapshot(rt, &c, &db), &before, "duplicates in the file");

    // Cancelled while reading the file, or while writing: left as it was.
    for phase in ["reading", "writing"] {
        let cancel = never();
        let flag = Arc::clone(&cancel);
        let e = rt.block_on(import(&c, &import_options(&db, "precious", dups.clone(), true), cancel, |p| {
            if matches!((phase, p), ("reading", Progress::Checked(_)) | ("writing", Progress::Written(_))) {
                flag.store(true, Ordering::Relaxed);
            }
        }))
        .unwrap_err()
        .to_string();
        let expected = if phase == "reading" { "cancelled: nothing was imported" } else { "cancelled: precious was left as it was" };
        assert_eq!(e, expected);
        assert_same_db(&snapshot(rt, &c, &db), &before, &format!("cancelled while {phase}"));
    }
    // Without "drop first", a cancel says what was added.
    let cancel = never();
    let flag = Arc::clone(&cancel);
    let e = rt.block_on(import(&c, &import_options(&db, "scratch", dups.clone(), false), cancel, |p| {
        if matches!(p, Progress::Written(_)) {
            flag.store(true, Ordering::Relaxed);
        }
    }))
    .unwrap_err()
    .to_string();
    assert_eq!(e, "cancelled after 1000 documents were imported");
    assert_eq!(find_all(rt, &c, &db, "scratch").len(), 1000);
    run(rt, &c, &db, doc! { "drop": "scratch" });

    // Done: replaced by the documents of the file, without the indexes and
    // options of the old one, like a drop. The view stays.
    let good = file("good.json", b"{\"_id\":\"a\"}\n{\"_id\":\"b\"}\n".to_vec());
    let done = rt.block_on(import(&c, &import_options(&db, "precious", good.clone(), true), never(), |_| {})).unwrap();
    assert_eq!((done.inserted, done.failed), (2, 0));
    let after = snapshot(rt, &c, &db);
    assert_eq!(after.keys().collect::<Vec<_>>(), ["precious", "precious_view"], "no temporary collection left");
    assert_eq!(find_all(rt, &c, &db, "precious"), vec![doc! { "_id": "a" }, doc! { "_id": "b" }]);
    assert_eq!(after["precious"].indexes.len(), 1);
    assert_eq!(after["precious"].options, doc! {});
    assert_eq!(after["precious_view"], before["precious_view"]);
    // Over a view: a collection takes its place.
    rt.block_on(import(&c, &import_options(&db, "precious_view", good.clone(), true), never(), |_| {})).unwrap();
    assert_eq!(snapshot(rt, &c, &db)["precious_view"].kind, "collection");
    assert_eq!(find_all(rt, &c, &db, "precious_view").len(), 2);

    // Big documents: each batch stays under the size of a command; the
    // largest document MongoDB takes goes in.
    let mut big = String::new();
    for i in 0..15 {
        big.push_str(&format!("{{\"_id\":{i},\"s\":\"{}\"}}\n", "x".repeat(500 * 1024)));
    }
    big.push_str(&format!("{{\"_id\":\"big\",\"s\":\"{}\"}}\n{{\"_id\":\"after\"}}\n", "y".repeat(9 * 1024 * 1024)));
    let largest = 16 * 1024 * 1024 - bson::to_vec(&doc! { "_id": "largest", "s": "" }).unwrap().len();
    big.push_str(&format!("{{\"_id\":\"largest\",\"s\":\"{}\"}}\n", "z".repeat(largest)));
    let path = file("big.json", big.into_bytes());
    for drop_first in [false, true] {
        let done = rt.block_on(import(&c, &import_options(&db, "big", path.clone(), drop_first), never(), |_| {})).unwrap();
        assert_eq!((done.inserted, done.failed), (18, 0), "drop {drop_first}");
        assert_eq!(find_all(rt, &c, &db, "big").len(), 18);
        if !drop_first {
            run(rt, &c, &db, doc! { "drop": "big" });
        }
    }
    run(rt, &c, &db, doc! { "dropDatabase": 1 });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imports_never_lose_a_collection_on_a_modern_server() {
    if !live() {
        return;
    }
    import_safety_suite(&rt(), &modern_tools());
}

#[test]
fn imports_never_lose_a_collection_on_a_legacy_server() {
    if !live() {
        return;
    }
    import_safety_suite(&rt(), &legacy_tools());
}

/// A restore that does not complete leaves the database as it was: a
/// damaged dump changes nothing; "drop first" replaces a collection only
/// with its complete copy, and a collection the server refuses is left as
/// it was while the others are restored.
fn restore_safety_suite(rt: &tokio::runtime::Runtime, t: &Tools) {
    use rustyrobo::dump::{DumpOptions, dump, restore};
    use rustyrobo::transfer::Progress;
    use std::sync::atomic::{AtomicBool, Ordering};
    let c = open(rt, &t.settings).unwrap();
    let db = format!("rr_e2e_restore_safety_{}", t.name);
    let dir = temp(&format!("restore-safety-{}", t.name));
    seed(rt, &c, &db);
    let dumped = snapshot(rt, &c, &db);
    let folder = DumpOptions { db: db.clone(), collections: Vec::new(), dir: dir.clone(), gzip: false, archive: false };
    let archive = DumpOptions { archive: true, ..folder.clone() };
    let gz_archive = DumpOptions { archive: true, gzip: true, ..folder.clone() };
    for o in [&folder, &archive, &gz_archive] {
        rt.block_on(dump(&c, o, &AtomicBool::new(false), |_| {})).unwrap();
    }
    // The database as it is when restored over: changed since the dump.
    let change = || {
        run(rt, &c, &db, doc! { "update": "many", "updates": [{ "q": { "_id": 1 }, "u": { "$set": { "email": "changed@x" } } }] });
        run(rt, &c, &db, doc! { "delete": "types", "deletes": [{ "q": { "_id": "second" }, "limit": 1 }] });
        insert(rt, &c, &db, "later", &[doc! { "_id": "not in the dump" }]);
        run(rt, &c, &db, doc! { "collMod": "adults", "viewOn": "many", "pipeline": [{ "$match": { "age": { "$lt": 5 } } }] });
        if !c.is_legacy() {
            insert(rt, &c, &db, "ts", &[doc! { "_id": "later", "t": bson::DateTime::from_millis(1_800_000_000_000), "m": "s9", "v": 0.0 }]);
        }
    };
    change();
    let before = snapshot(rt, &c, &db);
    let restoring = |from: &std::path::Path, drop_first: bool| rt.block_on(restore(&c, &restore_options(&db, from.to_path_buf(), drop_first), never(), |_| {}));

    // Damaged dumps change nothing, whatever is damaged.
    let damaged = |name: &str, from: &std::path::Path, change: &dyn Fn(&std::path::Path)| -> std::path::PathBuf {
        let to = dir.join("damaged").join(name);
        let _ = std::fs::remove_dir_all(&to);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        if from.is_dir() {
            std::fs::create_dir_all(&to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let e = e.unwrap();
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        } else {
            std::fs::copy(from, &to).unwrap();
        }
        change(&to);
        to
    };
    let cut = |path: &std::path::Path, n: usize| {
        let bytes = std::fs::read(path).unwrap();
        std::fs::write(path, &bytes[..bytes.len() - n]).unwrap();
    };
    let archive_bytes = std::fs::read(archive.target()).unwrap();
    let doc_of_many = bson::to_vec(&find_all(rt, &c, &db, "many")[2000]).unwrap();
    // In the text of the last field: still a document, another one.
    let flip_at = archive_bytes.windows(doc_of_many.len()).position(|w| w == doc_of_many).expect("a document of many in the archive") + doc_of_many.len() - 4;
    let last_eof = archive_bytes.windows(4).rposition(|w| w == [0xff; 4]).unwrap();
    let cases: Vec<(&str, std::path::PathBuf, &str)> = vec![
        ("a .bson file cut short", damaged("cut", &folder.target(), &|d| cut(&d.join("many.bson"), 10)), "truncated file"),
        ("a .bson file with garbage", damaged("garbage", &folder.target(), &|d| std::fs::write(d.join("capped.bson"), b"\x10\x00\x00\x00garbage!!!!!").unwrap()), "capped.bson"),
        ("a metadata file that is not JSON", damaged("metadata", &folder.target(), &|d| std::fs::write(d.join("adults.metadata.json"), "{ nope").unwrap()), "invalid JSON"),
        ("an archive with a changed byte", damaged("flipped.archive", &archive.target(), &|p| {
            let mut b = std::fs::read(p).unwrap();
            b[flip_at] ^= 0x01;
            std::fs::write(p, b).unwrap();
        }), "many"),
        ("an archive cut between two blocks", damaged("cut.archive", &archive.target(), &|p| std::fs::write(p, &archive_bytes[..last_eof - 60]).unwrap()), "truncated"),
        ("a gzipped archive cut short", damaged("cut.archive.gz", &gz_archive.target(), &|p| cut(p, 100)), "archive"),
    ];
    for (what, from, expected) in &cases {
        for drop_first in [true, false] {
            let e = format!("{:#}", restoring(from, drop_first).unwrap_err());
            assert!(e.contains(expected), "{what}, drop {drop_first}: {e}");
            assert_same_db(&snapshot(rt, &c, &db), &before, &format!("{what}, drop {drop_first}"));
        }
    }

    // Cancelled while reading the dump, or while writing: left as it was.
    for from in [folder.target(), archive.target()] {
        for phase in ["reading", "writing"] {
            let cancel = never();
            let flag = Arc::clone(&cancel);
            let e = rt
                .block_on(restore(&c, &restore_options(&db, from.clone(), true), cancel, |p| {
                    if matches!((phase, p), ("reading", Progress::Checked(_)) | ("writing", Progress::Written(_))) {
                        flag.store(true, Ordering::Relaxed);
                    }
                }))
                .unwrap_err()
                .to_string();
            assert!(e.starts_with("cancelled"), "{e}");
            let restored = snapshot(rt, &c, &db);
            for (name, kept) in &before {
                // What was restored before the cancel is as dumped, the rest as it was.
                assert!(restored.get(name) == Some(kept) || dumped.get(name) == restored.get(name), "cancelled while {phase}: {name} is neither as it was nor as dumped");
            }
            assert!(restored.keys().all(|k| !k.contains(".rr-tmp-")), "no temporary collection left: {:?}", restored.keys());
            if restored != before {
                seed(rt, &c, &db);
                change();
                assert_same_db(&snapshot(rt, &c, &db), &before, "state before, made again");
            }
        }
    }

    // The server refuses a collection: it is left as it was, the others are restored.
    let refusals: Vec<(&str, std::path::PathBuf, &str, &str)> = vec![
        ("options the server refuses", damaged("options", &folder.target(), &|d| std::fs::write(d.join("many.metadata.json"), r#"{"options":{"capped":true},"indexes":[],"collectionName":"many","type":"collection"}"#).unwrap()), "many", "InvalidOptions"),
        ("a document refused", damaged("dup", &folder.target(), &|d| {
            let mut bytes = std::fs::read(d.join("many.bson")).unwrap();
            bytes.extend(bson::to_vec(&doc! { "_id": 5, "email": "dup@x" }).unwrap());
            std::fs::write(d.join("many.bson"), bytes).unwrap();
        }), "many", "1 document refused (E11000"),
        ("an index the documents break", damaged("index", &folder.target(), &|d| {
            let path = d.join("many.metadata.json");
            let text = std::fs::read_to_string(&path).unwrap().replace(r#""name":"sparse""#, r#""name":"sparse","unique":true"#);
            std::fs::write(path, text).unwrap();
        }), "many", "indexes: "),
    ];
    for (what, from, coll, expected) in &refusals {
        let r = restoring(from, true).unwrap();
        assert_eq!(r.problems.len(), 1, "{what}: {}", r.summary());
        assert!(r.problems[0].starts_with(&format!("{coll}: ")) && r.problems[0].contains(expected) && r.problems[0].ends_with("left as it was"), "{what}: {}", r.problems[0]);
        assert!(r.incomplete());
        let after = snapshot(rt, &c, &db);
        for (name, kept) in &after {
            let expected = if name == coll || name == "later" { &before[name] } else { &dumped[name] };
            assert_eq!(kept, expected, "{what}: {name}");
        }
        assert_eq!(after.keys().collect::<Vec<_>>(), before.keys().collect::<Vec<_>>(), "{what}");
    }

    // Complete: as dumped, and what the dump does not hold stays.
    for from in [folder.target(), archive.target(), gz_archive.target()] {
        let r = restoring(&from, true).unwrap();
        assert!(!r.incomplete(), "{}", r.summary());
        let mut expected = dumped.clone();
        expected.insert("later".into(), before["later"].clone());
        assert_same_db(&snapshot(rt, &c, &db), &expected, &format!("restored from {}", from.display()));
    }
    // Without "drop first": added to what is there, the duplicates and the
    // views that exist told.
    let r = restoring(&folder.target(), false).unwrap();
    assert!(r.incomplete());
    assert_eq!(r.docs.inserted, 0, "{}", r.summary());
    if c.is_legacy() {
        assert!(r.problems.contains(&"adults: exists, left as it was".to_string()), "{:?}", r.problems);
    }
    // The duplicate key error of a collation is not UTF-8 on 4.2: the
    // collection is told, the others restored.
    let collated = r.problems.iter().filter(|p| p.starts_with("collated: ")).count();
    assert_eq!(r.problems.len(), collated + usize::from(c.is_legacy()), "{:?}", r.problems);

    run(rt, &c, &db, doc! { "dropDatabase": 1 });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restores_never_lose_a_collection_on_a_modern_server() {
    if !live() {
        return;
    }
    restore_safety_suite(&rt(), &modern_tools());
}

#[test]
fn restores_never_lose_a_collection_on_a_legacy_server() {
    if !live() {
        return;
    }
    restore_safety_suite(&rt(), &legacy_tools());
}

/// The dumps of each mongodump, also of another server version: the names
/// of their files read right, what the server cannot take reported.
#[test]
fn dumps_of_each_mongodump_are_restored() {
    if !live() {
        return;
    }
    use rustyrobo::dump::restore;
    let rt = rt();
    let fixture = |p: &str| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(p);
    for t in [modern_tools(), legacy_tools()] {
        let c = open(&rt, &t.settings).unwrap();
        let db = format!("rr_e2e_fixtures_{}", t.name);
        for tool in ["mongodb-8", "mongodb-7", "mongodb-4.2"] {
            for from in ["rr_fixture", "gzip/rr_fixture", "rr_fixture.archive", "rr_fixture.archive.gz"] {
                let what = format!("{tool}/{from} into {}", t.name);
                run(&rt, &c, &db, doc! { "dropDatabase": 1 });
                // "x y" is another collection than "x+y": a 4.2 dump writes "x+y" as it is.
                insert(&rt, &c, &db, "x y", &[doc! { "_id": "other" }]);
                let r = rt.block_on(restore(&c, &restore_options(&db, fixture(&format!("{tool}/{from}")), true), never(), |_| {})).unwrap();
                let s = snapshot(&rt, &c, &db);
                assert_eq!(find_all(&rt, &c, &db, "x y"), vec![doc! { "_id": "other" }], "{what}");
                assert_eq!(find_all(&rt, &c, &db, "x+y").len(), 1, "{what}");
                assert_eq!(find_all(&rt, &c, &db, "a/b c").len(), 1, "{what}");
                assert_eq!(find_all(&rt, &c, &db, "users").len(), 3, "{what}");
                assert_eq!(s["users"].indexes.len(), 3, "{what}");
                assert_eq!(s["adults"].kind, "view", "{what}");
                assert_eq!(s["capped"].options.get_bool("capped"), Ok(true), "{what}");
                assert_eq!(find_all(&rt, &c, &db, "system.js").len(), 1, "{what}");
                if tool != "mongodb-4.2" && c.is_legacy() {
                    // No time series before MongoDB 5.0.
                    assert_eq!(r.problems.len(), 1, "{what}: {}", r.summary());
                    assert!(r.problems[0].starts_with("ts: "), "{what}: {}", r.problems[0]);
                    assert!(!s.contains_key("ts"));
                } else {
                    assert!(r.problems.is_empty(), "{what}: {}", r.summary());
                }
                if tool != "mongodb-4.2" && !c.is_legacy() {
                    let ts = find_all(&rt, &c, &db, "ts");
                    assert_eq!(ts.iter().map(|d| d.get_f64("v").unwrap_or_else(|_| d.get_i32("v").unwrap() as f64)).collect::<Vec<_>>(), [1.0, 2.0, 3.0], "{what}: the measurements of the buckets");
                }
            }
        }
        run(&rt, &c, &db, doc! { "dropDatabase": 1 });
    }
}

/// With the privileges of readWrite only: what needs more (documents
/// against their validator, buckets of time series before MongoDB 8.2) is
/// refused before anything is dropped.
#[test]
fn restores_with_read_write_privileges_only() {
    if !live() {
        return;
    }
    use rustyrobo::dump::{DumpOptions, dump, restore};
    let rt = rt();
    let admin = open(&rt, &mongo8()).unwrap();
    let db = "rr_e2e_read_write";
    seed(&rt, &admin, db);
    let dir = temp("read-write");
    let o = DumpOptions { db: db.into(), collections: Vec::new(), dir: dir.clone(), gzip: false, archive: false };
    rt.block_on(dump(&admin, &o, &std::sync::atomic::AtomicBool::new(false), |_| {})).unwrap();
    let before = snapshot(&rt, &admin, db);
    let _ = rt.block_on(admin.run_command(db, doc! { "dropUser": "rr_rw" }, None));
    run(&rt, &admin, db, doc! { "createUser": "rr_rw", "pwd": "pw", "roles": [{ "role": "readWrite", "db": db }] });
    let mut s = mongo8();
    s.credentials.database = db.into();
    s.credentials.username = "rr_rw".into();
    s.credentials.password = "pw".into();
    let rw = open(&rt, &s).unwrap();

    let r = rt.block_on(restore(&rw, &restore_options(db, o.target(), true), never(), |_| {})).unwrap();
    let after = snapshot(&rt, &admin, db);
    // The document its validator refuses keeps the collection as it was.
    assert!(r.problems.iter().any(|p| p.starts_with("valid: 1 document refused (Document failed validation") && p.ends_with("left as it was")), "{:?}", r.problems);
    assert_eq!(after["valid"], before["valid"]);
    if at_least(&rw, 8, 2) {
        assert!(!r.problems.iter().any(|p| p.starts_with("ts:")), "rawData takes the buckets: {:?}", r.problems);
    } else {
        assert!(r.problems.iter().any(|p| p.starts_with("ts: Unauthorized")), "{:?}", r.problems);
    }
    assert_eq!(after["ts"].docs, before["ts"].docs, "the time series is never lost");
    assert_eq!(after.len(), before.len());

    // A dump made with readWrite: the measurements of the time series,
    // when its buckets cannot be read, restored all the same.
    let o = DumpOptions { dir: dir.join("rw"), ..o };
    std::fs::create_dir_all(&o.dir).unwrap();
    rt.block_on(dump(&rw, &o, &std::sync::atomic::AtomicBool::new(false), |_| {})).unwrap();
    let copy = "rr_e2e_read_write_copy";
    run(&rt, &admin, copy, doc! { "dropDatabase": 1 });
    let r = rt.block_on(restore(&admin, &restore_options(copy, o.target(), true), never(), |_| {})).unwrap();
    assert!(r.problems.is_empty(), "{}", r.summary());
    // Put in buckets again, the measurements come back with their fields in
    // another order.
    let fields_in_order = |docs: &[Vec<u8>]| -> Vec<Vec<(String, Bson)>> {
        docs.iter()
            .map(|raw| {
                let mut fields: Vec<(String, Bson)> = bson::Document::from_reader(raw.as_slice()).unwrap().into_iter().collect();
                fields.sort_by(|a, b| a.0.cmp(&b.0));
                fields
            })
            .collect()
    };
    assert_eq!(fields_in_order(&snapshot(&rt, &admin, copy)["ts"].docs), fields_in_order(&before["ts"].docs));
    drop(rw);
    run(&rt, &admin, db, doc! { "dropUser": "rr_rw" });
    for d in [db, copy] {
        run(&rt, &admin, d, doc! { "dropDatabase": 1 });
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_command_works_like_in_the_mongo_shell() {
    if !live() {
        return;
    }
    let rt = rt();
    for settings in [mongo8(), mongo42()] {
        let c = Arc::new(open(&rt, &settings).unwrap());
        let mut sh = Shell::new(&rt, c, "rr_run_command", Duration::from_secs(10));
        sh.ok("db.c.drop(); db.c.insertMany([{ a: 1 }, { a: 2 }, { a: 3 }])");

        // On a database, with a document or a name.
        assert_eq!(sh.text("db.runCommand({ ping: 1 }).ok"), "1");
        assert_eq!(sh.text("db.runCommand('ping').ok"), "1");
        assert_eq!(sh.text("db.runReadCommand({ ping: 1 }).ok"), "1");
        assert_eq!(sh.text("db.adminCommand('ping').ok"), "1");
        assert_eq!(sh.text("db.runCommand({ count: 'c', query: { a: { $gt: NumberLong(1) } } }).n"), "2");
        assert_eq!(sh.text("db.getMongo().adminCommand({ ping: 1 }).ok"), "1");
        assert_eq!(sh.text("db.getMongo().runCommand('admin', { ping: 1 }).ok"), "1");

        // On a collection: a name applies to the collection.
        assert_eq!(sh.text("db.c.runCommand('count').n"), "3");
        assert_eq!(sh.text("db.getCollection('c').runCommand('count', { query: { a: 1 } }).n"), "1");
        assert_eq!(sh.text("db.c.runCommand({ count: 'c' }).n"), "3");
        assert_eq!(sh.text("db.c.runReadCommand('count').n"), "3");

        // A refused command returns the reply of the server, and the script goes on.
        let outs = sh.run("var r = db.runCommand({ nope: 1 }); print(r.ok, r.code, r.codeName); print(r.errmsg.includes('nope'))");
        let texts: Vec<_> = outs.iter().map(|o| match &o.kind {
            OutputKind::Text(t) => t.clone(),
            other => panic!("unexpected output {other:?}"),
        }).collect();
        assert_eq!(texts, ["0 59 CommandNotFound", "true"]);
        let OutputKind::Docs { docs, .. } = sh.ok("db.runCommand({ nope: 1 })") else { panic!("expected a document") };
        let reply = docs[0].as_document().unwrap();
        assert_eq!(reply.get_i32("code").unwrap(), 59);
        assert!(reply.get_str("errmsg").unwrap().contains("nope"));
        assert_eq!(sh.text("db.adminCommand({ nope: 1 }).ok"), "0");
        assert_eq!(sh.text("db.c.runCommand('nope').ok"), "0");
        // Mistakes in the script still stop it, and so do the helpers of the shell.
        let outs = sh.run("db.runCommand(null)");
        assert!(matches!(&outs[0].kind, OutputKind::Error(e) if e.contains("command document")), "{:?}", outs[0].kind);
        let outs = sh.run("rs.status()");
        assert!(matches!(&outs[0].kind, OutputKind::Error(e) if e.contains("NoReplicationEnabled")), "{:?}", outs[0].kind);

        // Cursors opened by a command, continued and killed in the following ones.
        assert_eq!(sh.text("var r = db.runCommand({ find: 'c', batchSize: 1 }); db.runCommand({ getMore: r.cursor.id, collection: 'c', batchSize: 1 }).cursor.nextBatch.length"), "1");
        assert_eq!(sh.text("var r = db.runCommand({ find: 'c', batchSize: 1 }); db.runCommand({ killCursors: 'c', cursors: [r.cursor.id] }).cursorsKilled.length"), "1");
        sh.ok("db.dropDatabase()");
    }
}

#[test]
fn non_latin_text_round_trips() {
    if !live() {
        return;
    }
    let rt = rt();
    for (settings, db) in [(mongo8(), "rr_e2e_utf8_modern"), (mongo42(), "rr_e2e_utf8_legacy")] {
        let c = Arc::new(open(&rt, &settings).unwrap());
        let mut sh = Shell::new(&rt, c, db, Duration::from_secs(20));
        sh.ok("db.dropDatabase()");
        for text in ["שלום עולם", "שָׁלוֹם", "مرحبا بالعالم", "你好世界", "한국어", "😀"] {
            sh.ok(&format!("db.t.insertOne({{ _id: '{text}', '{text}': '{text}' }})"));
            // Fetched natively by the driver, then through the JS bridge.
            let d = sh.docs(&format!("db.t.find({{ _id: '{text}' }})"));
            assert_eq!(field(&d[0], text), &Bson::String(text.into()));
            assert_eq!(sh.text(&format!("db.t.findOne({{ _id: '{text}' }})['{text}']")), text);
        }
        sh.ok("db.dropDatabase()");
    }
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
    sh.handle.execute("while (true) {}".into(), DropVars::All);
    std::thread::sleep(Duration::from_millis(300));
    sh.handle.interrupt();
    let reply = sh.rx.recv_timeout(Duration::from_secs(5)).expect("interrupted in time");
    assert!(matches!(&reply.outputs[0].kind, OutputKind::Error(e) if e.contains("interrupted")), "{:?}", reply.outputs);
    assert!(started.elapsed() < Duration::from_secs(5));
    // sleep() also honours the interruption.
    sh.handle.execute("sleep(20000); 1".into(), DropVars::All);
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

#[test]
fn latest_release_on_github_is_understood() {
    if !live() {
        return;
    }
    rustyrobo::update::check().expect("reply of GitHub");
}

/// Real files of a published release, whatever the system running the test:
/// the layout of the archives is what matters.
fn install_published(asset: &str, exe: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rustyrobo-install-{asset}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let exe = dir.join(exe);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "old program").unwrap();
    let release = rustyrobo::update::Release { version: "1.2.0".into(), notes: String::new(), asset: Some(asset.into()) };
    let mut last = (0, None);
    rustyrobo::update::install(&release, &exe, &std::sync::atomic::AtomicBool::new(false), |done, total| last = (done, total)).expect("install");
    assert!(last.0 > 1_000_000 && last.1 == Some(last.0), "progress: {last:?}");
    assert!(std::fs::metadata(&exe).unwrap().len() > 1_000_000, "replaced");
    let staged = std::fs::read_dir(exe.parent().unwrap()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".rustyrobo-update"));
    assert!(!staged, "no leftovers");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o111, 0, "executable");
    }
    exe
}

#[test]
fn update_installs_the_files_of_a_release() {
    if !live() {
        return;
    }
    install_published("rustyrobo-v1.2.0-linux-x86_64.tar.gz", "rustyrobo");
    install_published("rustyrobo-v1.2.0-windows-x86_64.zip", "rustyrobo.exe");
    // A bundle is replaced whole, under the name the user gave it.
    let exe = install_published("rustyrobo-v1.2.0-macos-aarch64.zip", "Renamed.app/Contents/MacOS/rustyrobo");
    let contents = exe.parent().unwrap().parent().unwrap();
    assert!(contents.join("Info.plist").is_file());
    assert!(contents.join("_CodeSignature").is_dir());
}

#[test]
fn update_refuses_a_file_with_another_checksum() {
    if !live() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("rustyrobo-install-sums-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("rustyrobo");
    std::fs::write(&exe, "old program").unwrap();
    // A file of the release that SHA256SUMS does not list: itself.
    let release = rustyrobo::update::Release { version: "1.2.0".into(), notes: String::new(), asset: Some("SHA256SUMS".into()) };
    let e = rustyrobo::update::install(&release, &exe, &std::sync::atomic::AtomicBool::new(false), |_, _| {}).unwrap_err();
    assert!(e.contains("No checksum"), "{e}");
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old program");
}
