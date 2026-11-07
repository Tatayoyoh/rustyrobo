//! Query history: the scripts executed in the shells, newest last, with
//! their connection and database.
//!
//! Stored apart from the settings, in `history.json` (0600: scripts may hold
//! sensitive values), so that saving the settings does not rewrite it.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Entries kept; the oldest go first.
pub const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryEntry {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    pub connection: Option<Uuid>,
    /// Name of the connection when executed: shown once it is deleted.
    pub connection_name: String,
    pub db: String,
    pub script: String,
}

impl Default for HistoryEntry {
    fn default() -> Self {
        Self { id: Uuid::new_v4(), at: Utc::now(), connection: None, connection_name: String::new(), db: String::new(), script: String::new() }
    }
}

impl HistoryEntry {
    /// First non-empty line, for lists.
    pub fn first_line(&self) -> &str {
        self.script.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default()
    }

    fn same_query(&self, other: &HistoryEntry) -> bool {
        self.script.trim() == other.script.trim() && self.db == other.db && self.connection == other.connection
    }

    /// True when every word of `query` is in the script, the database or
    /// the connection name, ignoring case.
    pub fn matches(&self, query: &str) -> bool {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return true;
        }
        let hay = format!("{}\n{}\n{}", self.script, self.db, self.connection_name).to_lowercase();
        words.iter().all(|w| hay.contains(w.as_str()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub entries: Vec<HistoryEntry>,
}

pub fn history_path() -> PathBuf {
    crate::settings::config_dir().join("history.json")
}

impl History {
    pub fn load() -> Self {
        std::fs::read_to_string(history_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = history_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Adds an execution. The same query executed again moves to the end
    /// instead of being listed twice.
    pub fn push(&mut self, entry: HistoryEntry) {
        if entry.script.trim().is_empty() {
            return;
        }
        self.entries.retain(|e| !e.same_query(&entry));
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let extra = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..extra);
        }
    }

    pub fn remove(&mut self, id: Uuid) {
        self.entries.retain(|e| e.id != id);
    }

    pub fn get(&self, id: Uuid) -> Option<&HistoryEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Indices of the entries matching `query`, newest first.
    pub fn search(&self, query: &str) -> Vec<usize> {
        (0..self.entries.len()).rev().filter(|&i| self.entries[i].matches(query)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(script: &str, db: &str) -> HistoryEntry {
        HistoryEntry { script: script.into(), db: db.into(), ..Default::default() }
    }

    #[test]
    fn repeated_queries_move_to_the_end() {
        let mut h = History::default();
        h.push(entry("db.a.find({})", "shop"));
        h.push(entry("db.b.find({})", "shop"));
        h.push(entry("  db.a.find({})\n", "shop"));
        h.push(entry("db.a.find({})", "logs"));
        h.push(entry("   ", "shop"));
        let shape: Vec<(&str, &str)> = h.entries.iter().map(|e| (e.first_line(), e.db.as_str())).collect();
        assert_eq!(shape, vec![("db.b.find({})", "shop"), ("db.a.find({})", "shop"), ("db.a.find({})", "logs")]);
    }

    #[test]
    fn oldest_entries_are_dropped() {
        let mut h = History::default();
        for i in 0..MAX_ENTRIES + 5 {
            h.push(entry(&format!("db.c.find({{ i: {i} }})"), "x"));
        }
        assert_eq!(h.entries.len(), MAX_ENTRIES);
        assert_eq!(h.entries[0].script, "db.c.find({ i: 5 })");
    }

    #[test]
    fn search_is_newest_first_and_ignores_case() {
        let mut h = History::default();
        h.push(entry("db.users.find({ name: 'Ada' })", "shop"));
        h.push(HistoryEntry { connection_name: "Prod".into(), ..entry("db.orders.count()", "shop") });
        h.push(entry("db.users.drop()", "logs"));
        assert_eq!(h.search(""), vec![2, 1, 0]);
        assert_eq!(h.search("USERS"), vec![2, 0]);
        assert_eq!(h.search("users shop"), vec![0], "every word");
        assert_eq!(h.search("prod"), vec![1], "connection name");
        let id = h.entries[1].id;
        assert!(h.get(id).is_some());
        h.remove(id);
        assert!(h.get(id).is_none());
        assert_eq!(h.entries.len(), 2);
    }

    #[test]
    fn first_line() {
        assert_eq!(entry("\n  // all\n  db.a.find()", "x").first_line(), "// all");
        assert_eq!(entry("", "x").first_line(), "");
    }
}
