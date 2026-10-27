//! Embedded mongo shell.
//!
//! Each query tab owns a [`ShellHandle`]: a dedicated thread running a
//! QuickJS context. The variables a script declares can be kept for the next
//! executions, all or some of them (see [`DropVars`]). The JavaScript API
//! lives in `prelude.js` and reaches MongoDB through a single native
//! `runCommand` bridge exchanging canonical EJSON.

pub mod split;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bson::{Bson, Document};
use crossbeam_channel::{Receiver, Sender, unbounded};
use rquickjs::context::EvalOptions;
use rquickjs::{Context, Ctx, Function, Runtime, Value};
use tokio::runtime::Handle;

use crate::mongo::{Connection, Session, bson_as_i64};

const PRELUDE: &str = include_str!("prelude.js");

/// A `find` that can be re-run with another skip/limit by the result pager.
#[derive(Debug, Clone, PartialEq)]
pub struct QuerySpec {
    pub db: String,
    pub coll: String,
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub skip: u64,
    /// 0 = no limit.
    pub limit: i64,
    pub hint: Option<Bson>,
    pub collation: Option<Document>,
}

impl QuerySpec {
    fn from_bson(d: &Document) -> Option<Self> {
        Some(QuerySpec {
            db: d.get_str("db").ok()?.to_string(),
            coll: d.get_str("coll").ok()?.to_string(),
            filter: d.get_document("filter").cloned().unwrap_or_default(),
            projection: d.get_document("projection").ok().cloned(),
            sort: d.get_document("sort").ok().cloned(),
            skip: d.get("skip").and_then(bson_as_i64).unwrap_or(0).max(0) as u64,
            limit: d.get("limit").and_then(bson_as_i64).unwrap_or(0),
            hint: d.get("hint").filter(|h| !matches!(h, Bson::Null)).cloned(),
            collation: d.get_document("collation").ok().cloned(),
        })
    }

    pub fn find_command(&self, skip: u64, limit: u64) -> Document {
        let mut cmd = bson::doc! { "find": &self.coll, "filter": self.filter.clone() };
        if let Some(p) = &self.projection {
            cmd.insert("projection", p.clone());
        }
        if let Some(s) = &self.sort {
            cmd.insert("sort", s.clone());
        }
        if skip > 0 {
            cmd.insert("skip", skip as i64);
        }
        cmd.insert("limit", limit as i64);
        cmd.insert("batchSize", limit as i64);
        cmd.insert("singleBatch", true);
        if let Some(h) = &self.hint {
            cmd.insert("hint", h.clone());
        }
        if let Some(c) = &self.collation {
            cmd.insert("collation", c.clone());
        }
        cmd
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // few instances, short-lived
pub enum OutputKind {
    Docs {
        docs: Vec<Bson>,
        spec: Option<QuerySpec>,
        /// Namespace for cursors that cannot be paged (aggregate…).
        ns: Option<String>,
        more: bool,
    },
    Text(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub statement: String,
    pub line: usize,
    pub kind: OutputKind,
    pub elapsed: Duration,
}

/// A global variable declared by the user's scripts.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellVar {
    pub name: String,
    /// Short description of the value: `Array(12)`, `"text"`, `42`…
    pub summary: String,
}

#[derive(Debug)]
pub struct ShellReply {
    pub request: u64,
    pub outputs: Vec<ShellOutput>,
    pub current_db: String,
    pub elapsed: Duration,
    /// Variables declared by the scripts, after this execution.
    pub vars: Vec<ShellVar>,
}

/// Variables of the earlier executions to drop before running a script.
#[derive(Debug, Clone, PartialEq)]
pub enum DropVars {
    /// Every script starts from a clean state.
    All,
    /// Keep the variables, except these ones.
    Only(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct ShellOptions {
    pub batch_size: u32,
    pub timeout: Duration,
    pub load_mongorc: bool,
}

enum Request {
    Execute { id: u64, script: String, drop: DropVars },
    SetDb(String),
}

pub struct ShellHandle {
    tx: Sender<Request>,
    interrupt: Arc<AtomicBool>,
    next_id: u64,
}

impl ShellHandle {
    pub fn spawn(
        conn: Arc<Connection>,
        rt: Handle,
        db: String,
        options: ShellOptions,
        reply: impl Fn(ShellReply) + Send + 'static,
    ) -> Self {
        let (tx, rx) = unbounded();
        let interrupt = Arc::new(AtomicBool::new(false));
        let flag = interrupt.clone();
        std::thread::Builder::new()
            .name("mongo-shell".into())
            .spawn(move || shell_thread(conn, rt, db, options, flag, rx, reply))
            .expect("spawn shell thread");
        ShellHandle { tx, interrupt, next_id: 1 }
    }

    pub fn execute(&mut self, script: String, drop: DropVars) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.interrupt.store(false, Ordering::SeqCst);
        let _ = self.tx.send(Request::Execute { id, script, drop });
        id
    }

    pub fn set_db(&self, db: String) {
        let _ = self.tx.send(Request::SetDb(db));
    }

    pub fn interrupt(&self) {
        self.interrupt.store(true, Ordering::SeqCst);
    }
}

struct NativeState {
    conn: Arc<Connection>,
    rt: Handle,
    session: Option<Session>,
    interrupt: Arc<AtomicBool>,
    timeout: Duration,
    prints: Vec<String>,
    batch: u32,
}

fn block_on_interruptible<T>(
    st: &NativeState,
    fut: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let flag = st.interrupt.clone();
    let timeout = st.timeout;
    st.rt.block_on(async move {
        let watch = async {
            while !flag.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        };
        tokio::select! {
            r = tokio::time::timeout(timeout, fut) => match r {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!(
                    "Operation timed out after {} s (Options > Change Shell Timeout…)",
                    timeout.as_secs()
                )),
            },
            _ = watch => Err(anyhow::anyhow!("Execution interrupted by user")),
        }
    })
}

fn ejson_to_document(json: &str) -> Result<Document, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    match Bson::try_from(v).map_err(|e| e.to_string())? {
        Bson::Document(d) => Ok(d),
        other => Err(format!("expected a document, got {other}")),
    }
}

pub fn ejson_to_bson(json: &str) -> Result<Bson, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Bson::try_from(v).map_err(|e| e.to_string())
}

fn install_natives<'js>(ctx: &Ctx<'js>, state: Option<Rc<RefCell<NativeState>>>, batch: u32) -> rquickjs::Result<()> {
    let g = ctx.globals();
    let st = state.clone();
    g.set(
        "__n_cmd",
        Function::new(ctx.clone(), move |db: String, json: String| -> String {
            let Some(st) = &st else {
                return "\u{1}No connection".into();
            };
            let cmd = match ejson_to_document(&json) {
                Ok(c) => c,
                Err(e) => return format!("\u{1}Invalid command: {e}"),
            };
            let mut guard = st.borrow_mut();
            let state = &mut *guard;
            let conn = state.conn.clone();
            let mut session = state.session.take();
            let res = block_on_interruptible(state, async {
                if session.is_none() {
                    session = conn.start_session().await.ok();
                }
                conn.run_command(&db, cmd, session.as_mut()).await
            });
            state.session = session;
            match res {
                Ok(doc) => Bson::Document(doc).into_canonical_extjson().to_string(),
                Err(e) => format!("\u{1}{}", crate::mongo::error_message(&e)),
            }
        })?,
    )?;
    let st = state.clone();
    g.set(
        "__n_print",
        Function::new(ctx.clone(), move |text: String| {
            if let Some(st) = &st {
                st.borrow_mut().prints.push(text);
            }
        })?,
    )?;
    g.set(
        "__n_oid",
        Function::new(ctx.clone(), || bson::oid::ObjectId::new().to_hex())?,
    )?;
    let st = state;
    g.set(
        "__n_sleep",
        Function::new(ctx.clone(), move |ms: f64| {
            let deadline = Instant::now() + Duration::from_millis(ms.max(0.0) as u64);
            while Instant::now() < deadline {
                if st.as_ref().is_some_and(|s| s.borrow().interrupt.load(Ordering::SeqCst)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })?,
    )?;
    g.set("__n_batch", Function::new(ctx.clone(), move || batch)?)?;
    Ok(())
}

fn eval_opts(filename: &str) -> EvalOptions {
    let mut o = EvalOptions::default();
    o.global = true;
    o.strict = false;
    o.filename = Some(filename.to_string());
    o
}

fn exception_message(ctx: &Ctx<'_>, err: rquickjs::Error) -> String {
    if let rquickjs::Error::Exception = err {
        let e = ctx.catch();
        let fmt: rquickjs::Result<Function> = ctx.globals().get("__fmtErr");
        if let Ok(f) = fmt
            && let Ok(s) = f.call::<_, String>((e.clone(),))
        {
            return if s.contains("InternalError: interrupted") {
                "Execution interrupted by user".into()
            } else {
                s
            };
        }
        if let Some(ex) = e.as_exception() {
            return ex.message().unwrap_or_else(|| "exception".into());
        }
        return format!("{e:?}");
    }
    err.to_string()
}

fn syntax_ok(ctx: &Ctx<'_>, src: &str) -> bool {
    let check: rquickjs::Result<Function> = ctx.globals().get("Function");
    let Ok(f) = check else { return true };
    let ok = f.call::<_, Value>((src,)).is_ok();
    if !ok {
        let _ = ctx.catch();
    }
    ok
}

fn shell_thread(
    conn: Arc<Connection>,
    rt: Handle,
    db: String,
    options: ShellOptions,
    interrupt: Arc<AtomicBool>,
    rx: Receiver<Request>,
    reply: impl Fn(ShellReply),
) {
    let runtime = match Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rustyrobo: QuickJS runtime: {e}");
            return;
        }
    };
    runtime.set_memory_limit(1 << 30);
    let flag = interrupt.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || flag.load(Ordering::SeqCst))));
    let context = Context::full(&runtime).expect("QuickJS context");
    let state = Rc::new(RefCell::new(NativeState {
        conn,
        rt,
        session: None,
        interrupt,
        timeout: options.timeout,
        prints: Vec::new(),
        batch: options.batch_size.max(1),
    }));

    context.with(|ctx| {
        install_natives(&ctx, Some(state.clone()), options.batch_size).expect("natives");
        if let Err(e) = ctx.eval_with_options::<(), _>(PRELUDE, eval_opts("<prelude>")) {
            eprintln!("rustyrobo: prelude error: {}", exception_message(&ctx, e));
        }
        set_db(&ctx, &db);
        if options.load_mongorc
            && let Some(home) = directories::BaseDirs::new()
        {
            let rc = home.home_dir().join(".mongorc.js");
            if let Ok(src) = std::fs::read_to_string(&rc)
                && let Err(e) = ctx.eval_with_options::<(), _>(src, eval_opts(".mongorc.js"))
            {
                state
                    .borrow_mut()
                    .prints
                    .push(format!(".mongorc.js: {}", exception_message(&ctx, e)));
            }
        }
        let _ = ctx.eval::<(), _>("__markBaseline()");
    });

    while let Ok(req) = rx.recv() {
        match req {
            Request::SetDb(name) => context.with(|ctx| set_db(&ctx, &name)),
            Request::Execute { id, script, drop } => {
                let started = Instant::now();
                let outputs = context.with(|ctx| {
                    drop_vars(&ctx, &drop);
                    run_script(&ctx, &state, &script)
                });
                let (current_db, vars) = context.with(|ctx| {
                    (ctx.eval::<String, _>("db.getName()").unwrap_or_default(), user_vars(&ctx))
                });
                reply(ShellReply { request: id, outputs, current_db, elapsed: started.elapsed(), vars });
            }
        }
    }
}

fn set_db(ctx: &Ctx<'_>, name: &str) {
    let name = serde_json::to_string(name).unwrap_or_else(|_| "\"test\"".into());
    let _ = ctx.eval::<(), _>(format!("globalThis.db = __makeDB({name});"));
}

fn drop_vars(ctx: &Ctx<'_>, drop: &DropVars) {
    let names = match drop {
        DropVars::All => "null".to_string(),
        DropVars::Only(names) if names.is_empty() => return,
        DropVars::Only(names) => serde_json::to_string(names).unwrap_or_else(|_| "[]".into()),
    };
    let _ = ctx.eval::<(), _>(format!("__dropVars({names})"));
}

fn user_vars(ctx: &Ctx<'_>) -> Vec<ShellVar> {
    let json = ctx.eval::<String, _>("__varsInfo()").unwrap_or_default();
    let list: Vec<(String, String)> = serde_json::from_str(&json).unwrap_or_default();
    list.into_iter().map(|(name, summary)| ShellVar { name, summary }).collect()
}

/// Evaluates a statement as indirect `eval` code: its `var`s and functions
/// become deletable globals (a script's `var`s and functions would not be).
fn eval_statement<'js>(ctx: &Ctx<'js>, code: String) -> rquickjs::Result<Value<'js>> {
    let eval: Function = ctx.globals().get("__eval")?;
    eval.call((code,))
}

fn run_script(ctx: &Ctx<'_>, state: &Rc<RefCell<NativeState>>, script: &str) -> Vec<ShellOutput> {
    let mut outputs = Vec::new();
    let mut statements = split::split_statements(script);
    let flush_prints = |outputs: &mut Vec<ShellOutput>, stmt: &split::Statement| {
        let prints = std::mem::take(&mut state.borrow_mut().prints);
        if !prints.is_empty() {
            outputs.push(ShellOutput {
                statement: stmt.text.clone(),
                line: stmt.line,
                kind: OutputKind::Text(prints.join("\n")),
                elapsed: Duration::ZERO,
            });
        }
    };

    let mut i = 0;
    while i < statements.len() {
        let rewritten = split::rewrite_shell_command(&statements[i].text);
        if rewritten.is_none() && !syntax_ok(ctx, &statements[i].text) {
            // The heuristic split may have cut a statement: merge forward
            // until it parses, or give up and report the original error.
            let backup = statements.clone();
            let mut merged = false;
            while split::merge_with_next(&mut statements, i) {
                if syntax_ok(ctx, &statements[i].text) {
                    merged = true;
                    break;
                }
            }
            if !merged {
                statements = backup;
            }
        }
        let stmt = statements[i].clone();
        let code = rewritten
            .or_else(|| split::declarations_to_var(&stmt.text))
            .unwrap_or_else(|| stmt.text.clone());
        let t0 = Instant::now();
        let result = eval_statement(ctx, code)
            .and_then(|v| {
                let f: Function = ctx.globals().get("__shellResult")?;
                f.call::<_, String>((v,))
            });
        flush_prints(&mut outputs, &stmt);
        match result {
            Ok(json) => {
                let kind = match resolve_native(state, &json) {
                    Some(Ok(kind)) => Some(kind),
                    Some(Err(msg)) => {
                        let elapsed = t0.elapsed();
                        outputs.push(ShellOutput { statement: stmt.text.clone(), line: stmt.line, kind: OutputKind::Error(msg), elapsed });
                        break;
                    }
                    None => parse_result(&json),
                };
                let elapsed = t0.elapsed();
                if let Some(kind) = kind {
                    outputs.push(ShellOutput { statement: stmt.text.clone(), line: stmt.line, kind, elapsed });
                }
            }
            Err(e) => {
                let elapsed = t0.elapsed();
                let msg = exception_message(ctx, e);
                outputs.push(ShellOutput {
                    statement: stmt.text.clone(),
                    line: stmt.line,
                    kind: OutputKind::Error(msg),
                    elapsed,
                });
                break; // like the mongo shell: stop at the first error
            }
        }
        i += 1;
    }
    outputs
}

/// Results the JavaScript side hands over unexecuted (`find`) or as an open
/// cursor (`aggregate`) are fetched here with the driver: documents stay
/// BSON instead of round-tripping through Extended JSON and QuickJS, which
/// dominates the time for large documents.
fn resolve_native(state: &Rc<RefCell<NativeState>>, json: &str) -> Option<Result<OutputKind, String>> {
    if !json.starts_with(r#"{"t":"query""#) && !json.starts_with(r#"{"t":"cursor""#) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut guard = state.borrow_mut();
    let st = &mut *guard;
    let batch = st.batch as u64;
    let conn = st.conn.clone();
    match v.get("t")?.as_str()? {
        "query" => {
            let mut spec = Bson::try_from(v.get("spec")?.clone()).ok()?.as_document().and_then(QuerySpec::from_bson)?;
            // limit(-n) means a single batch of n documents.
            spec.limit = spec.limit.abs();
            let page = if spec.limit > 0 { batch.min(spec.limit as u64) } else { batch };
            let fetch_spec = spec.clone();
            let res = block_on_interruptible(st, async move {
                // One extra document tells whether there is a next page.
                fetch_page(&conn, &fetch_spec, fetch_spec.skip, page + 1).await
            });
            Some(
                res.map(|mut docs| {
                    let more = docs.len() as u64 > page;
                    docs.truncate(page as usize);
                    OutputKind::Docs { docs, spec: Some(spec), ns: None, more }
                })
                .map_err(|e| crate::mongo::error_message(&e)),
            )
        }
        "cursor" => {
            let db = v.get("db")?.as_str()?.to_string();
            let coll = v.get("coll")?.as_str()?.to_string();
            let ns = v.get("ns").and_then(|n| n.as_str()).map(String::from);
            let id = Bson::try_from(v.get("id")?.clone()).ok().and_then(|b| bson_as_i64(&b))?;
            let mut session = st.session.take();
            let res = block_on_interruptible(st, async {
                let mut docs: Vec<Bson> = Vec::new();
                let mut id = id;
                while id != 0 && (docs.len() as u64) < batch {
                    let want = (batch - docs.len() as u64) as i64;
                    let more = conn
                        .run_command(&db, bson::doc! { "getMore": id, "collection": &coll, "batchSize": want }, session.as_mut())
                        .await?;
                    let cur = more.get_document("cursor")?;
                    if let Ok(batch_docs) = cur.get_array("nextBatch") {
                        docs.extend(batch_docs.iter().cloned());
                    }
                    id = cur.get("id").and_then(bson_as_i64).unwrap_or(0);
                }
                if id != 0 {
                    let _ = conn.run_command(&db, bson::doc! { "killCursors": &coll, "cursors": [id] }, session.as_mut()).await;
                }
                Ok((docs, id != 0))
            });
            st.session = session;
            Some(res.map(|(docs, more)| OutputKind::Docs { docs, spec: None, ns, more }).map_err(|e| crate::mongo::error_message(&e)))
        }
        _ => None,
    }
}

fn parse_result(json: &str) -> Option<OutputKind> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    match v.get("t")?.as_str()? {
        "none" => None,
        "text" => Some(OutputKind::Text(v.get("text")?.as_str()?.to_string())),
        "docs" => {
            let docs = v
                .get("docs")?
                .as_array()?
                .iter()
                .filter_map(|d| Bson::try_from(d.clone()).ok())
                .collect();
            let spec = v
                .get("spec")
                .and_then(|s| Bson::try_from(s.clone()).ok())
                .and_then(|b| b.as_document().and_then(QuerySpec::from_bson));
            let ns = v.get("ns").and_then(|n| n.as_str()).map(String::from);
            let more = v.get("more").and_then(|m| m.as_bool()).unwrap_or(false);
            Some(OutputKind::Docs { docs, spec, ns, more })
        }
        _ => None,
    }
}

/// Evaluates a shell-syntax value (`{ _id: ObjectId("…"), d: ISODate(…) }`)
/// without a connection. Used by the document editor dialogs.
pub fn parse_shell_value(src: &str) -> Result<Bson, String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    context.with(|ctx| {
        install_natives(&ctx, None, 50).map_err(|e| e.to_string())?;
        ctx.eval_with_options::<(), _>(PRELUDE, eval_opts("<prelude>"))
            .map_err(|e| exception_message(&ctx, e))?;
        // No line break before `src`: error positions match the user's lines.
        let code = format!("__toEJSONString(({src}\n))");
        let json: String = ctx
            .eval_with_options(code, eval_opts("<document>"))
            .map_err(|e| exception_message(&ctx, e))?;
        ejson_to_bson(&json)
    })
}

/// Fetches one page of a `find` for the result pager.
pub async fn fetch_page(conn: &Connection, spec: &QuerySpec, skip: u64, limit: u64) -> anyhow::Result<Vec<Bson>> {
    let mut limit = limit.max(1);
    if spec.limit > 0 {
        // Respect the user's limit: never page beyond it.
        let end = spec.skip + spec.limit as u64;
        if skip >= end {
            return Ok(Vec::new());
        }
        limit = limit.min(end - skip);
    }
    let docs = crate::mongo::collect_cursor(conn, &spec.db, spec.find_command(skip, limit), limit as usize, None).await?;
    Ok(docs.into_iter().map(Bson::Document).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shell_document() {
        let b = parse_shell_value(
            r#"{ _id: ObjectId("5f1d7a0e1c9d440000a1b2c3"), n: 1, f: 1.5, l: NumberLong(5),
                 d: ISODate("2020-01-02T03:04:05Z"), u: UUID("0123456789abcdef0123456789abcdef"),
                 r: /ab+c/i, a: [1, "x"], z: null, big: NumberLong("9007199254740993") }"#,
        )
        .unwrap();
        let d = b.as_document().unwrap();
        assert_eq!(d.keys().next().unwrap(), "_id");
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(d.get("n"), Some(&Bson::Int32(1)));
        assert_eq!(d.get("f"), Some(&Bson::Double(1.5)));
        assert_eq!(d.get("l"), Some(&Bson::Int64(5)));
        assert_eq!(d.get("big"), Some(&Bson::Int64(9007199254740993)));
        assert!(matches!(d.get("d"), Some(Bson::DateTime(_))));
        assert!(matches!(d.get("u"), Some(Bson::Binary(b)) if b.subtype == bson::spec::BinarySubtype::Uuid));
        assert!(matches!(d.get("r"), Some(Bson::RegularExpression(_))));
        assert_eq!(d.get("z"), Some(&Bson::Null));
    }

    #[test]
    fn reports_syntax_errors() {
        assert!(parse_shell_value("{ a: }").is_err());
        let e = parse_shell_value("{\n    a: 1\n    \"b\" 2\n}").unwrap_err();
        assert!(e.contains(":3:"), "line of the user's text: {e}");
    }

    /// Evaluates an expression with the prelude loaded (no connection) and
    /// returns its value converted to a string by JavaScript.
    fn js(src: &str) -> Result<String, String> {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            install_natives(&ctx, None, 50).unwrap();
            ctx.eval_with_options::<(), _>(PRELUDE, eval_opts("<prelude>")).map_err(|e| exception_message(&ctx, e))?;
            ctx.eval_with_options::<String, _>(format!("String((() => {{ return {src}; }})())"), eval_opts("<t>"))
                .map_err(|e| exception_message(&ctx, e))
        })
    }

    #[test]
    fn tojson_shell_format() {
        assert_eq!(js("tojson({ a: 1, b: [1, 'x'], c: {} })").unwrap(), "{\n    \"a\" : 1,\n    \"b\" : [\n        1,\n        \"x\"\n    ],\n    \"c\" : {}\n}");
        assert_eq!(js("tojsononeline({ a: ObjectId('5f1d7a0e1c9d440000a1b2c3') })").unwrap(), "{ \"a\" : ObjectId(\"5f1d7a0e1c9d440000a1b2c3\") }");
        assert_eq!(js("tojson(ISODate('2020-01-02'))").unwrap(), "ISODate(\"2020-01-02T00:00:00.000Z\")");
        assert_eq!(js("tojson(NumberLong('9007199254740993'))").unwrap(), "NumberLong(\"9007199254740993\")");
        assert_eq!(js("tojson(NumberLong(5))").unwrap(), "NumberLong(5)");
        assert_eq!(js("tojson(UUID('01234567-89ab-cdef-0123-456789abcdef'))").unwrap(), "UUID(\"01234567-89ab-cdef-0123-456789abcdef\")");
        assert_eq!(js("tojson(Timestamp(1, 2))").unwrap(), "Timestamp(1, 2)");
        assert_eq!(js("tojson([MinKey, MaxKey])").unwrap(), "[\n    MinKey,\n    MaxKey\n]");
    }

    #[test]
    fn bson_helpers() {
        assert_eq!(js("ObjectId('5f1d7a0e1c9d440000a1b2c3').getTimestamp().toISOString()").unwrap(), "2020-07-26T12:41:50.000Z");
        assert_eq!(js("ObjectId().str.length").unwrap(), "24");
        assert_eq!(js("new ObjectId('5F1D7A0E1C9D440000A1B2C3').str").unwrap(), "5f1d7a0e1c9d440000a1b2c3");
        assert_eq!(js("ObjectId('5f1d7a0e1c9d440000a1b2c3') instanceof ObjectId").unwrap(), "true");
        assert!(js("ObjectId('nope')").unwrap_err().contains("Invalid ObjectId"));
        assert_eq!(js("ObjectId.isValid('5f1d7a0e1c9d440000a1b2c3')").unwrap(), "true");
        assert_eq!(js("ISODate('2020-01-02 03:04:05').toISOString()").unwrap(), "2020-01-02T03:04:05.000Z");
        assert!(js("ISODate('garbage')").unwrap_err().contains("invalid ISO date"));
        assert_eq!(js("HexData(0, 'ff00').base64()").unwrap(), "/wA=");
        assert_eq!(js("BinData(0, '/wA=').hex()").unwrap(), "ff00");
        assert_eq!(js("UUID().tojson().length").unwrap(), "44");
        assert_eq!(js("NumberInt('12.7').valueOf()").unwrap(), "12");
    }

    #[test]
    fn ejson_bridge() {
        assert_eq!(js("__toEJSONString({ a: 1, b: 1.5, c: 2 ** 40, d: undefined, e: [undefined] })").unwrap(),
            r#"{"a":{"$numberInt":"1"},"b":{"$numberDouble":"1.5"},"c":{"$numberDouble":"1099511627776.0"},"e":[null]}"#);
        assert_eq!(js("__toEJSONString(/a.b/gi)").unwrap(), r#"{"$regularExpression":{"pattern":"a.b","options":"i"}}"#);
        assert_eq!(js("JSON.stringify(__fromEJ({ $numberLong: '5' }).s)").unwrap(), "\"5\"");
        assert_eq!(js("__fromEJ({ $date: { $numberLong: '0' } }).toISOString()").unwrap(), "1970-01-01T00:00:00.000Z");
        assert_eq!(js("__fromEJ({ $regularExpression: { pattern: 'a', options: 'x' } }).tojson()").unwrap(), "/a/x");
        // _id is put first on insert, like the mongo shell.
        assert_eq!(js("Object.keys(__withId({ a: 1 })).join(',')").unwrap(), "_id,a");
    }

    #[test]
    fn db_and_collection_proxies() {
        assert_eq!(js("db.getName()").unwrap(), "test");
        assert_eq!(js("db.users.getFullName()").unwrap(), "test.users");
        assert_eq!(js("db.system.js.getFullName()").unwrap(), "test.system.js");
        assert_eq!(js("db.getSiblingDB('other').coll.getFullName()").unwrap(), "other.coll");
        assert_eq!(js("db.getCollection('a b').getName()").unwrap(), "a b");
        assert_eq!(js("(__use('admin'), db.getName())").unwrap(), "admin");
    }

    #[test]
    fn query_spec_building() {
        let spec = js("JSON.stringify(__toEJ(db.getCollection('items').find({ a: 1 }, { b: 1 }).sort({ n: -1 }).skip(5).limit(10).hint('a_1').__spec()))").unwrap();
        let b = ejson_to_bson(&spec).unwrap();
        let q = QuerySpec::from_bson(b.as_document().unwrap()).unwrap();
        assert_eq!(q.db, "test");
        assert_eq!(q.coll, "items");
        assert_eq!(q.filter, bson::doc! { "a": 1 });
        assert_eq!(q.projection, Some(bson::doc! { "b": 1 }));
        assert_eq!(q.sort, Some(bson::doc! { "n": -1 }));
        assert_eq!((q.skip, q.limit), (5, 10));
        assert_eq!(q.hint, Some(Bson::String("a_1".into())));
        let cmd = q.find_command(20, 50);
        assert_eq!(cmd.get_i64("skip").unwrap(), 20);
        assert_eq!(cmd.get_i64("limit").unwrap(), 50);
        assert!(cmd.get_bool("singleBatch").unwrap());
        assert_eq!(cmd.keys().next().unwrap(), "find");
        // A string filter means { _id: … }.
        assert_eq!(js("JSON.stringify(db.c.find('x')._filter)").unwrap(), r#"{"_id":"x"}"#);
        // Modifying an executed query is rejected.
        assert!(js("(() => { const q = db.c.find(); q._cursor = {}; return q.sort({a:1}); })()").unwrap_err().contains("already executed"));
    }

    #[test]
    fn shell_results() {
        assert_eq!(js("__shellResult(undefined)").unwrap(), r#"{"t":"none"}"#);
        assert_eq!(js("__shellResult('hi')").unwrap(), r#"{"t":"text","text":"hi"}"#);
        assert_eq!(js("__shellResult(42)").unwrap(), r#"{"t":"text","text":"42"}"#);
        assert_eq!(js("__shellResult(db.users)").unwrap(), r#"{"t":"text","text":"test.users"}"#);
        assert_eq!(js("JSON.parse(__shellResult({ a: 1 })).docs.length").unwrap(), "1");
        assert_eq!(js("JSON.parse(__shellResult([{ a: 1 }, { a: 2 }])).docs.length").unwrap(), "2");
        let k = parse_result(&js("__shellResult({ a: NumberLong(3) })").unwrap()).unwrap();
        match k {
            OutputKind::Docs { docs, spec, more, .. } => {
                assert_eq!(docs, vec![Bson::Document(bson::doc! { "a": 3i64 })]);
                assert!(spec.is_none() && !more);
            }
            other => panic!("{other:?}"),
        }
        assert!(parse_result(r#"{"t":"none"}"#).is_none());
        assert!(parse_result("garbage").is_none());
    }

    #[test]
    fn commands_without_connection_fail_cleanly() {
        let e = js("db.c.findOne()").unwrap_err();
        assert!(e.contains("MongoServerError") && e.contains("No connection"), "{e}");
        assert!(js("db.c.updateOne({}, { a: 1 })").unwrap_err().contains("atomic operators"));
        assert!(js("db.c.replaceOne({}, { $set: { a: 1 } })").unwrap_err().contains("must not contain"));
        assert!(js("db.killOp()").unwrap_err().contains("operation id"));
        assert!(js("__show('nope')").unwrap_err().contains("don't know how to show"));
    }

    #[test]
    fn errors_are_formatted() {
        let e = js("null.x").unwrap_err();
        assert!(e.starts_with("TypeError"), "{e}");
        let e = js("(() => { throw 'plain' })()").unwrap_err();
        assert_eq!(e, "Error: \"plain\"");
    }
    /// Runs scripts statement by statement in one context without connection,
    /// like the shell thread does, and returns the last value as a string.
    fn with_shell(f: impl FnOnce(&mut dyn FnMut(&str, DropVars) -> Result<String, String>, &dyn Fn() -> Vec<String>)) {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            install_natives(&ctx, None, 50).unwrap();
            ctx.eval_with_options::<(), _>(PRELUDE, eval_opts("<prelude>")).unwrap();
            ctx.eval::<(), _>("__markBaseline()").unwrap();
            let mut run = |script: &str, drop: DropVars| {
                drop_vars(&ctx, &drop);
                let mut last = String::new();
                for stmt in split::split_statements(script) {
                    let code = split::declarations_to_var(&stmt.text).unwrap_or(stmt.text);
                    let v = eval_statement(&ctx, code).map_err(|e| exception_message(&ctx, e))?;
                    let string: Function = ctx.globals().get("String").unwrap();
                    last = string.call((v,)).unwrap();
                }
                Ok(last)
            };
            let vars = || user_vars(&ctx).into_iter().map(|v| format!("{}={}", v.name, v.summary)).collect();
            f(&mut run, &vars);
        });
    }

    #[test]
    fn scripts_can_run_again() {
        with_shell(|run, vars| {
            let script = "const a = [1, 2]\nlet s = new Set(a)\nclass K { get n() { return s.size } }\nfunction f() { return new K().n }\nf()";
            assert_eq!(run(script, DropVars::Only(vec![])).unwrap(), "2");
            assert_eq!(run(script, DropVars::Only(vec![])).unwrap(), "2");
            assert_eq!(vars(), vec!["a=Array(2)", "s=Set(2)", "K=function", "f=function"]);
            // Internals and the prelude's globals are not user variables.
            assert_eq!(run("it = 1; tojson = tojson; __x = 2", DropVars::Only(vec![])).unwrap(), "2");
            assert_eq!(vars().len(), 5);
        });
    }

    #[test]
    fn variables_can_be_dropped() {
        with_shell(|run, vars| {
            run("const a = 1, b = 'x'.repeat(50); c = { k: 1 }", DropVars::All).unwrap();
            assert_eq!(vars(), vec!["a=1", format!("b=\"{}…", "x".repeat(38)).as_str(), "c=Object(1)"]);
            assert_eq!(run("typeof a + typeof b", DropVars::Only(vec!["a".into()])).unwrap(), "undefinedstring");
            assert_eq!(run("typeof b + typeof c + typeof db", DropVars::All).unwrap(), "undefinedundefinedobject");
            assert!(vars().is_empty());
            // Without dropping, values carry over.
            run("let n = 1", DropVars::All).unwrap();
            assert_eq!(run("n += 1; n", DropVars::Only(vec![])).unwrap(), "2");
            // Statements still see each other's lexical declarations.
            assert_eq!(run("const x = 2\nconst y = x * 3\ny", DropVars::All).unwrap(), "6");
        });
    }

    #[test]
    fn console_table() {
        with_shell(|run, _| {
            let t = run("console.table([{ a: 1, b: 'x' }, { a: 22 }])", DropVars::All);
            assert_eq!(t.unwrap(), "undefined");
        });
        let t = js("(() => { let out; const p = __n_print; globalThis.__n_print = (s) => (out = s); console.table([{ a: 1, b: 'x' }, { a: 22 }, 3], ['a']); globalThis.__n_print = p; return out; })()").unwrap();
        assert_eq!(
            t,
            "┌─────────┬────┬────────┐\n│ (index) │ a  │ Values │\n├─────────┼────┼────────┤\n│ 0       │ 1  │        │\n│ 1       │ 22 │        │\n│ 2       │    │ 3      │\n└─────────┴────┴────────┘"
        );
    }

    #[test]
    fn statement_errors_are_formatted() {
        with_shell(|run, _| {
            let e = run("let a = 1\nnull.x", DropVars::All).unwrap_err();
            assert!(e.starts_with("TypeError"), "{e}");
            assert_eq!(e, "TypeError: cannot read property 'x' of null\n    at <eval> (<shell>:1:1)");
        });
    }
}
