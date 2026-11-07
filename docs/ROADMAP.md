# Roadmap: diff with Studio 3T features

What Studio 3T offers, compared with rustyrobo. Checked items exist in rustyrobo; the others are
ideas to pick up later, with notes on how they would fit the architecture (`Action` → tokio task →
`AppEvent`, everything through `runCommand`, see `AGENT.md`).

Rustyrobo stays a light, Robo 3T-like tool: the enterprise features at the end of this list are
here for the record, not as a commitment.

## Already in rustyrobo

- [x] Embedded mongo shell, MongoDB 3.6 → 8.x (two driver backends)
- [x] Tree, table and text views of the results, with paging
- [x] Connection manager: replica sets, `mongodb+srv`, TLS, SCRAM / X.509 / PLAIN (LDAP), connection strings
- [x] SSH tunnels: password, encrypted keys, agent, host-key checks
- [x] Database explorer: databases, collections, views, indexes, users, stored functions
- [x] Collection management: create (capped, validator), rename, duplicate, copy to another database, drop
- [x] Index management: create / edit (unique, sparse, TTL, partial, collation, text, hidden), drop
- [x] User management: add, edit roles, drop
- [x] Document editing: insert, edit, delete, multi-selection
- [x] Editor: syntax highlighting, field-name and operator autocompletion, find, multi-cursor
- [x] Find in the results, copy values / names / paths
- [x] Server tools: server status, host info, replica set status, current operations, kill operation, logs, statistics
- [x] Favorite scripts manager
- [x] Colour themes, system fonts, right-to-left text
- [x] Self-update
- [x] **Import / export** in JSON (one document per line or array, relaxed or canonical Extended
  JSON) and CSV (chosen fields, delimiter, nested fields as `a.b`, type detection on import), from
  the explorer (collection, database) and from the results of a query; runs in the background,
  with progress and cancel
- [x] **Dump / restore** of a database in the formats of `mongodump`: folder (`.bson` + `.metadata.json`)
  or archive (`--archive`), gzip optional; options, indexes and views, chosen collections, drop first,
  into another database
- [x] **Query history**: every executed script with its connection and database, searchable,
  opened again in a new shell (Ctrl+H)
- [x] **Inline editing in the table view**: double-click a cell (or "Edit Value" in its menu) to
  change a top-level value in shell syntax, its type included (`NumberLong(5)`, `ISODate(…)`), or
  to add a field the document lacks; "Remove Field" unsets it. Sends `$set` / `$unset` on the
  `_id` of the row and reloads the page. Objects and arrays still go through Edit Document

## Queries and aggregation

- [ ] **Visual Explain**: show `explain("executionStats")` as a tree of stages (IXSCAN, FETCH,
  SORT…) with documents examined / returned and time per stage. A new result view for the reply of
  `explain`, no new backend work.
- [ ] **Aggregation editor**: one panel per stage (enable / disable, reorder by drag and drop) and
  the output of the pipeline up to the selected stage (`$limit` added for the preview). Generates
  `db.getCollection('x').aggregate([...])` for the shell.
- [ ] **Visual query builder**: build a `find` filter by dropping field names (from the sampled
  fields used by autocompletion) and choosing operators; generates the shell script.
- [ ] **SQL query**: `SELECT … FROM … WHERE … GROUP BY … ORDER BY` translated into `find` or
  `aggregate`. Needs an SQL parser crate (e.g. `sqlparser`) and a translation layer.
- [ ] **Query code**: translate a shell query into Java, Python, C#, Node.js, PHP, Ruby, Go.
- [ ] **Saved query parameters**: `$$name` placeholders filled in before execution.

## Data

- [ ] **Nested arrays and documents in the table view**: step into a sub-document or array column
  as a table of its own.
- [ ] [PRIORITIZED] **Copy / paste documents** between collections and servers (clipboard as Extended JSON).
- [ ] **Import modes**: upsert or merge by `_id` (`update` with `upsert: true`) besides insert;
  stop on first error.
- [ ] **More formats**: SQL `INSERT` statements, Excel; export of aggregation results.
- [ ] **Saved import / export tasks** to run again (see Tasks).
- [ ] **Compare & sync**: differences between two collections (by `_id`), then copy the differences one way.
- [ ] **Data masking**: copy a collection with fields hidden, hashed or replaced.
- [ ] **Reschema**: restructure documents (embed, flatten, rename fields) into a new collection.
- [ ] **SQL ↔ MongoDB migration**: from / to MySQL, PostgreSQL, SQL Server, Oracle.
- [ ] [PRIORITIZED] **GridFS**: list `fs.files`, download, upload, delete files.

## Schema and analysis

- [ ] **Schema explorer**: for each field of a sample, its types and how often, value
  distribution, with charts. Builds on `field_names_command` (server-side sampling).
- [ ] **Validation rules editor**: edit `$jsonSchema` of an existing collection (`collMod`),
  generated from the schema analysis.
- [ ] **Index usage**: `$indexStats` (accesses, since) next to each index; hide / unhide an index
  (`collMod`) from the explorer.
- [ ] **Charts**: bar / line / pie charts of aggregation results.

## Servers and administration

- [ ] **Role management**: custom roles (`createRole`, `updateRole`, privileges, inherited roles).
- [ ] **Live server monitoring**: opcounters, connections, memory, network, refreshed every few
  seconds from `serverStatus`, as charts.
- [ ] **Profiler**: turn the profiler on / off per database and browse `system.profile`.
- [ ] **Sharding view**: shards, chunks, balancer state.

## Connections

- [ ] **Kerberos (GSSAPI)** and **AWS IAM (`MONGODB-AWS`)** authentication (driver features to
  enable, plus the fields in the connection editor).
- [ ] **HTTP / SOCKS5 proxies**.
- [ ] [PRIORITIZED] **Read-only connections**: refuse write commands on a connection marked as production.

## Tools and teamwork

- [ ] **Tasks scheduler**: run saved imports, exports and scripts at set times.
- [ ] **AI assistant**: generate a query from a question in natural language.
- [ ] **Sharing**: export / import connections, favorite scripts and tasks to share with a team.
