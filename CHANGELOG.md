# Changelog

## 1.6.1

### Changed

- Results: large documents take about half the memory while they load

### Fixed

- Results: the memory taken by large results is given back after the next query, another page or closing the tab (it could stay at several hundred MB)
- Text view: a click in the text no longer keeps earlier results in memory
- Results: with large documents, a page showed only part of them (up to 16 MB) and no next page
- Explorer and dumps: databases of more than 101 collections no longer fail to load with "Cursor session id … is not the same…"

## 1.6.0

### Added

- Explorer: Ctrl+W closes the connection of the selected row
- Tree and table views: Shift+Up/Down select several documents
- Keyboard shortcuts: by section, with a search (Ctrl+F), scrolled in a small window
- New connections get a colour of their own
- Read-only connections: the commands that write are refused (for production servers)
- Update dialog: the changelog formatted
- Tree and table views: Ctrl+A selects all the rows shown

### Changed

- Connecting shows the explorer when it was hidden
- Results narrower than 800 px: the views (tree, table, text) and Export are in the section top right menu

## 1.5.0

### Added

- Tree, table and text views: PageUp/PageDown and Home/End; Up/Down scroll the text view
- Tree and table views: 
  - Enter on an object or an array opens the document on it, its block highlighted
  - Up/Down go through the rows, Right/Left open and close the nodes of the tree or go through the columns of the table, Enter edits
- Text view: the documents come by parts when scrolled to the end, instead of being left out past 2 MB
- Alt+arrows: go between the explorer, the editor, the results and the logs
- Ctrl+P: open a collection by its name
- Ctrl+K: next view (tree, table, text)
- Ctrl+J: favorite scripts
- View Document: Ctrl+E to edit
- Edit Document: Ctrl+S to save
- Results: double-click an `_id` to view the document, R and E view or edit the selected document
- Document modal: maximize (button, double-click on its title, Ctrl+M) and close button
- Document modal: Alt+F4 closes it, not the app
- Document modal: the eye of View in its title, Edit and Insert in the colours of the menus
- Explorer: Up/Down go through the rows, Right/Left open and close them, Enter opens a collection
- Explorer and tree view: Space opens and closes a node
- Tree view: a button and Shift+Space open the documents of the page, first level only, or close them
- Results: the number of documents of the query, next to the name of the collection
- Connecting (connection manager, welcome page) puts the keyboard in the explorer, on the server opened
- Query editor: a completed field name is followed by `:`, Ctrl+arrows stop at `:`, Ctrl+Shift+Left from a value selects its key
- Change default view into Preferences > Default view mode

### Changed

- Tree view: the documents start closed, a document alone open ("Auto expand first document" is off by default)
- "Auto expand first document": in the tree options (⋮ after the views), instead of the Options menu

### Fixed

- View Document and Edit Document: selecting text after right-to-left text copies the right text (#1)
- Text view: slow with large results
- Saving a document from the results took them back to their first page, the nodes closed and nothing selected

## 1.4.0

### Added
- Import and export: "Export Collection…" and "Import Documents…" in the menu of a collection, "Import Collection…" in the menus of a database
- Dump and restore database
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
