# Changelog

## Unreleased

### Added
- Import and export: "Export Collection…" and "Import Documents…" in the menu of a collection, "Import Collection…" in the menus of a database
- Dump and restore database
- Imports and restores keep your data safe: the whole file or dump is read before anything is written, so a damaged one changes nothing; "Drop first" fills a new collection that replaces the old one only once complete, so a failure, a refused document or a cancel leaves it as it was. A collection the server refuses is reported and the others are restored; "Done, but not completely" tells what was not
- Restore reads the dumps of each mongodump: names with `+` or spaces of mongodump 4.2 and 100, time series buckets as both name them, archives cut short (refused)
- Quitting while an import, export, dump or restore runs asks first, telling what stopping each one leaves; "Stop and Quit" waits for them to put back what they were replacing. An update installed meanwhile restarts only then
- Query history in File > Query History… or (Ctrl+H)
- Table and tree views: double-click a value for inline edit
- Context menus: icons, and action words coloured by kind (create, edit, delete)

### Fixed
- View Document: right-to-left sentences read in the right order, like in the results (#1)

## 1.3.0

### Added
- Preferences: choose the interface font and the monospace font among the installed fonts (#1)
- Preferences: fonts, font size and theme change right away; Cancel, Escape or closing the window restores them. Up and Down select the previous and next item of the focused list
- Preferences: font size slider
- Preferences: "Reset to Default" button
- View > Theme: a theme shows when hovered, and is kept when clicked
- Results: Ctrl+F finds in the tree, table and text views of the page, opening the nodes to show what it finds
- Results: Ctrl+C copies the value under the pointer, or the selection
- Results: Ctrl+Shift+C copies them with their names (`"name" : value`), also in the menus as "Copy Name and Value"
- Self-update: a chip in the toolbar tells when a new version is available, and "Download update and restart" downloads it, checks its checksum, replaces the program and restarts it. The file and the commands to update by hand stay available. Options > "Check for updates at startup" turns the check off
- Shell: `db.collection.runCommand()` and `runReadCommand()`, `db.runReadCommand()`, `db.getMongo().runCommand()` and `adminCommand()`

### Fixed
- Hebrew, Arabic, CJK, Thai, Indic and other scripts showed as boxes: the fonts installed on the system are now used for the characters missing from the bundled fonts (#1)
- Right-to-left text reads in the right order in the tree, table and text views, and a value too long for its column keeps its start visible
- `db.runCommand()` and `db.adminCommand()` return the reply of a refused command (`{ ok: 0, errmsg, code, codeName }`), like the mongo shell, instead of stopping the script
- `killCursors` was refused by the driver, also when the shell closed its own cursors

### Known issues
- Text views: on right-to-left lines, the highlight of a search match can be misplaced
- Editors (scripts, documents): right-to-left sentences show their words in reverse order and the
  cursor is off on these rows, which comes from egui (emilk/egui#1016, emilk/egui#8576)

## 1.2.0

### Added
- "Cross-script variables" toggle : keep the variables a script declares for the next executions
- `console.table()`, `console.debug()`, `console.trace()`

### Changed
- By default, each script execution starts with no variables from the previous ones

### Fixed
- Running a script that declares `const`/`let` again failed with `redeclaration of …`

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
