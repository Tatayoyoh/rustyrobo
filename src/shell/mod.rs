//! Embedded mongo shell.
//!
//! Each query tab owns a [`ShellHandle`]: a dedicated thread running a
//! QuickJS context (so variables persist between executions, like a real
//! shell). The JavaScript API lives in `prelude.js` and reaches MongoDB
//! through a single native `runCommand` bridge exchanging canonical EJSON.

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

#[derive(Debug)]
pub struct ShellReply {
    pub request: u64,
    pub outputs: Vec<ShellOutput>,
    pub current_db: String,
    pub elapsed: Duration,
}

#[derive(Debug, Clone)]
pub struct ShellOptions {
    pub batch_size: u32,
    pub timeout: Duration,
    pub load_mongorc: bool,
}

enum Request {
    Execute { id: u64, script: String },
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

    pub fn execute(&mut self, script: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.interrupt.store(false, Ordering::SeqCst);
        let _ = self.tx.send(Request::Execute { id, script });
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
                    "Operation timed out after {} s (Options → Change Shell Timeout…)",
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
    });

    while let Ok(req) = rx.recv() {
        match req {
            Request::SetDb(name) => context.with(|ctx| set_db(&ctx, &name)),
            Request::Execute { id, script } => {
                let started = Instant::now();
                let outputs = context.with(|ctx| run_script(&ctx, &state, &script));
                let current_db = context.with(|ctx| {
                    ctx.eval::<String, _>("db.getName()").unwrap_or_default()
                });
                reply(ShellReply { request: id, outputs, current_db, elapsed: started.elapsed() });
            }
        }
    }
}

fn set_db(ctx: &Ctx<'_>, name: &str) {
    let name = serde_json::to_string(name).unwrap_or_else(|_| "\"test\"".into());
    let _ = ctx.eval::<(), _>(format!("globalThis.db = __makeDB({name});"));
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
        let code = rewritten.unwrap_or_else(|| stmt.text.clone());
        let t0 = Instant::now();
        let result = ctx
            .eval_with_options::<Value, _>(code, eval_opts("<shell>"))
            .and_then(|v| {
                let f: Function = ctx.globals().get("__shellResult")?;
                f.call::<_, String>((v,))
            });
        let elapsed = t0.elapsed();
        flush_prints(&mut outputs, &stmt);
        match result {
            Ok(json) => {
                if let Some(kind) = parse_result(&json) {
                    outputs.push(ShellOutput { statement: stmt.text.clone(), line: stmt.line, kind, elapsed });
                }
            }
            Err(e) => {
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
        let code = format!("__toEJSONString((\n{src}\n))");
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
    }
}
