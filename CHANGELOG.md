# Changelog

## 1.1.0

### Added
- Favorite scripts manager: by connection and database, with drag and drop
- Multiple selection in tree and table views (Ctrl+click, Shift+click), Del to delete
- Editors: Ctrl+F search, Ctrl+D multiple cursors
- Document editor: live validation, foldable objects and arrays
- Foldable text result view
- Rename query tabs (double-click or context menu)
- Phosphor icons instead of emoji

### Changed
- egui 0.36: building from source needs Rust 1.95 or later

### Fixed
- F10 rotated the editor and results only one way: the editor then filled the whole area
- The selected connection was hard to see in the connection manager
- Dialogs: buttons at the bottom, height fitted to the content, single button for
  informational dialogs
- Line numbers of document syntax errors were off by one
- Line breaks in short string values showed as a box
- A dialog that settled on its size a frame late could stay misplaced until the next input

## 1.0.0

First release of Rustyrobo, a native MongoDB GUI written in Rust that ports Robomongo / Robo 3T.

### Like Robomongo
- Same interface: database explorer, query tabs, tree / table / text result views, paging, logs,
  menus and keyboard shortcuts (F5, F6, F2/F3/F4, Ctrl+T, Alt+1…9…)
- Embedded mongo shell: `db.getCollection('x').find()`, cursors, `aggregate`, CRUD, indexes,
  users, `use`, `show dbs`, `print`/`printjson`, `rs.*`, `sh.*`, `.mongorc.js`
- Document editing (view, edit, insert, delete) in shell syntax with validation
- Collection, index (TTL, partial, text), user and stored function management
- Import of Robo 3T connections

### Plus
- MongoDB 3.6 → 8.x: modern driver for 4.4+, automatic legacy driver for 3.6–4.2
- Reliable SSH tunnels: password, private key (encrypted keys detected), SSH agent,
  known_hosts verification
- Redesigned connection editor: sections with live status, inline validation, two-way
  connection string, step-by-step connection test; colour tags, groups, search
- Autocompletion of field names (sampled from the collection), query / update operators and
  aggregation stages
- Twelve colour themes, or follow the system
- Results fetched natively: large documents load an order of magnitude faster than through the
  shell

### Downloads
- Linux x86_64 / ARM64: `.tar.gz` (binary, `.desktop` file and icon)
- macOS Apple Silicon / Intel: `Rustyrobo.app` (unsigned: right-click → Open the first time)
- Windows x86_64: `.zip` (unsigned: SmartScreen may ask for confirmation)
