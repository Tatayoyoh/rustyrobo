# AGENT.md

Guide for coding agents working on **rustyrobo**, a native MongoDB GUI in Rust that ports
Robomongo / Robo 3T. Repository: https://github.com/Tatayoyoh/rustyrobo (Apache-2.0).

## Goals and constraints

- The UI mirrors Robo 3T (layout, menus, context menus, shortcuts, generated scripts like
  `db.getCollection('x').find({})`). Do not redesign screens without being asked. The connection
  manager and editor are the deliberate exceptions: they are improved versions.
- Supported servers: MongoDB 3.6 → 8.x.
- No Robomongo source code is copied. The original repo may be read for behaviour only.
- Keep the app light: no per-frame allocation of big data, no blocking on the UI thread.

## Commands

```sh
cargo build                     # dev build (deps are optimised, see Cargo.toml profiles)
cargo test                      # unit tests (must stay green)
cargo clippy --all-targets -- -D warnings   # CI fails on any warning
dev/ssh/gen-keys.sh && docker compose -f dev/docker-compose.yml up -d --wait
RR_LIVE=1 cargo test --test live --test ui -- --test-threads=1   # e2e on real servers: mongo 8, 4.2, SSH
RUSTYROBO_CONFIG_DIR=/tmp/rr RUSTYROBO_AUTOTEST=docs/screenshots cargo run   # UI walkthrough + screenshots
```

`RUSTYROBO_CONFIG_DIR` overrides the settings directory. Tests and experiments must never write into
the user's real `~/.config/rustyrobo` (settings, learned SSH host keys).

## Architecture

```
src/
  main.rs          window setup, icon
  app.rs           App state, Action → tokio task → AppEvent loop, shortcuts
  settings.rs      ConnectionSettings / AppSettings (JSON, 0600), Robo 3T import
  theme.rs         colour themes + egui Visuals; contrast-checked by tests
  bsonfmt.rs       BSON → shell syntax, short values, type names, legacy UUIDs
  autotest.rs      scripted UI walkthrough (RUSTYROBO_AUTOTEST)
  mongo/
    mod.rs         Connection: dual driver backend, connect diagnostics, run_command
    uri.rs         ConnectionSettings ↔ connection string
    ssh.rs         russh local port forwarding, known_hosts checks
  shell/
    mod.rs         per-tab QuickJS thread, natives, statement execution, paging
    prelude.js     mongo shell API (db, collections, cursors, BSON types) on runCommand
    split.rs       script → top-level statements; `use`/`show`/`it` rewriting
  ui/
    mod.rs         menus, toolbar, docks
    explorer.rs    database tree + context menus
    query.rs       query tabs (editor + results)
    output.rs      tree/table/text result panes, paging, document menu
    editor.rs      JS highlighting (memoised), line numbers, autocompletion
    connections.rs connection manager + editor (validation, URI, step-by-step test)
    dialogs.rs     other modals (documents, indexes, users, functions…)
```

Key design rules:

- **UI never blocks.** UI code pushes `Action`s; `App::run_action` spawns tokio tasks that send
  `AppEvent`s back; events are applied at the start of the next frame.
- **Everything goes through `runCommand`.** find/getMore/insert/listCollections… are commands. This
  keeps the two driver backends thin: `mongodb` 3.x (wire ≥ 9, MongoDB 4.4+) and `mongodb_legacy`
  2.8 (3.6–4.2), chosen automatically on a wire-version error. Both share `bson` 2.x.
- Shell cursors use an explicit session per shell so that `getMore` hits the same session.
- **Results are fetched natively.** A statement whose value is an unexecuted `find` (or an
  untouched `aggregate` cursor, opened with an empty first batch) is handed to Rust, which reads the
  page with the driver. Documents never round-trip through EJSON/QuickJS (13× faster on large
  documents, see `bench_large_documents`). Pipelines ending in `$out`/`$merge` keep a normal first
  batch, since the server only runs them then.
- The JS ↔ Rust bridge exchanges **canonical Extended JSON** strings. `serde_json` has
  `preserve_order` on: command key order matters (first key = command name).
- Through an SSH tunnel the driver connects to `127.0.0.1:<port>` with `directConnection=true`.

## Gotchas

- egui is 0.36 (needs Rust 1.95+): `eframe::App::ui(&mut self, ui, frame)`, `egui::Panel::{top,left,…}.show(ui, …)`,
  `egui::MenuBar::new().ui(…)`, `ui.close()` to close menus, `egui::Modal` for dialogs.
- Icons are Phosphor glyphs (`ui::icon::*`, regular variant only, loaded by
  `ui::install_icon_font`), never emoji. Any other symbol must exist in egui's bundled fonts
  (check with `fc-query` on `epaint_default_fonts/fonts/*`); missing ones render as boxes
  (e.g. `✎`, `⧉`, `→`, `⏎`).
- egui's "strong" text colour is `widgets.active.fg_stroke`: keep it equal to the theme text colour.
- A vertical `Separator` inside `horizontal_top` takes the whole available height: give it a size.
- In `prelude.js`, top-level `class X` creates a global lexical binding that shadows
  `globalThis.X`. Callable constructors (`ObjectId()`, `NumberLong()`…) are functions wrapping
  classes named `*T`.
- Each statement runs as indirect `eval` code (`__eval`, sloppy mode), with top-level `let`/`const`/
  `class` rewritten as `var` (`split::declarations_to_var`): user variables are then deletable
  globals. Before each execution, the shell drops all of them (default) or, with "Cross-script
  variables" on, only the ones unchecked in the tab's variable list. User variables = globals added
  after the prelude and `.mongorc.js` (`__markBaseline`), minus `__*` internals.
- Statement splitting is heuristic; chunks that fail to parse are merged with the next one.
- Theme colours must pass the WCAG checks in `theme::tests` (text ≥ 4.5:1, syntax ≥ 3:1).

## Conventions

- Match the surrounding code: comment density, naming, idioms. English in code, UI and docs.
- Tests, by layer (add tests with every change):
  - unit tests next to the code (`cargo test --lib`);
  - `tests/ui.rs`: headless UI e2e with `egui_kittest` (find widgets by label/role, click, type);
    offline tests always run, `live_*` ones need `RR_LIVE=1`;
  - `tests/live.rs`: backend e2e (shell API on 8.x and 4.2, SSH password/keys, errors).
  - SSH test keys are generated by `dev/ssh/gen-keys.sh` (git-ignored, mounted in `rr-ssh`).
- Large derived views (highlighting, tree rows, table columns, text) are cached; invalidate the
  cache instead of recomputing per frame.
- When the UI changes visibly, regenerate `docs/screenshots` with the autotest and keep `README.md`
  and the landing page (`docs/index.html`, Lucide icons, no emoji) in sync.
- Commits: conventional style (`feat:`, `fix:`, `test:`…).
