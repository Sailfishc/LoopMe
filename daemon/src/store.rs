//! SQLite storage. One connection behind a mutex; every call is short.

use crate::model::*;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

pub struct Store {
    conn: Mutex<Connection>,
}

/// A paired device with its secrets; `Device` is the public view.
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub public_key: [u8; 32],
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub is_local: bool,
    pub push_token: Option<String>,
    /// `sandbox` or `production`.
    pub push_env: Option<String>,
}

impl DeviceRecord {
    pub fn public(&self) -> Device {
        Device {
            id: self.id.clone(),
            name: self.name.clone(),
            created_at: self.created_at,
            last_seen_at: self.last_seen_at,
            is_local: self.is_local,
            has_push: self.push_token.is_some(),
        }
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY, seq INTEGER NOT NULL, role TEXT NOT NULL, text TEXT NOT NULL,
  item_id TEXT, decision_id TEXT, created_at INTEGER NOT NULL, streaming INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS messages_seq ON messages(seq);
CREATE TABLE IF NOT EXISTS items (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, detail TEXT NOT NULL, status TEXT NOT NULL,
  next_check_at INTEGER, done_when TEXT, source TEXT, checks INTEGER NOT NULL DEFAULT 0,
  unchanged_checks INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS decisions (
  id TEXT PRIMARY KEY, item_id TEXT, title TEXT NOT NULL, detail TEXT NOT NULL, kind TEXT NOT NULL,
  status TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, answered_at INTEGER);
CREATE TABLE IF NOT EXISTS memory (
  id TEXT PRIMARY KEY, text TEXT NOT NULL, author TEXT NOT NULL, status TEXT NOT NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS devices (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, public_key BLOB NOT NULL, created_at INTEGER NOT NULL,
  last_seen_at INTEGER, is_local INTEGER NOT NULL DEFAULT 0, push_token TEXT, push_env TEXT);
CREATE TABLE IF NOT EXISTS idempotency (client_id TEXT PRIMARY KEY, result TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS notifications (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, item_id TEXT, title TEXT NOT NULL, body TEXT NOT NULL,
  payload TEXT NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL, sent_at INTEGER);
CREATE TABLE IF NOT EXISTS codex_threads (id TEXT PRIMARY KEY, purpose TEXT NOT NULL, created_at INTEGER NOT NULL);
"#;

fn message_from(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        seq: r.get(1)?,
        role: r.get(2)?,
        text: r.get(3)?,
        item_id: r.get(4)?,
        decision_id: r.get(5)?,
        created_at: r.get(6)?,
        streaming: r.get::<_, i64>(7)? != 0,
    })
}
const MSG_COLS: &str = "id, seq, role, text, item_id, decision_id, created_at, streaming";

fn item_from(r: &Row) -> rusqlite::Result<Item> {
    Ok(Item {
        id: r.get(0)?,
        title: r.get(1)?,
        detail: r.get(2)?,
        status: r.get(3)?,
        next_check_at: r.get(4)?,
        done_when: r.get(5)?,
        source: r.get(6)?,
        checks: r.get(7)?,
        unchanged_checks: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}
const ITEM_COLS: &str =
    "id, title, detail, status, next_check_at, done_when, source, checks, unchanged_checks, created_at, updated_at";

fn decision_from(r: &Row) -> rusqlite::Result<Decision> {
    Ok(Decision {
        id: r.get(0)?,
        item_id: r.get(1)?,
        title: r.get(2)?,
        detail: r.get(3)?,
        kind: r.get(4)?,
        status: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        answered_at: r.get(8)?,
    })
}
const DEC_COLS: &str = "id, item_id, title, detail, kind, status, created_at, expires_at, answered_at";

fn memory_from(r: &Row) -> rusqlite::Result<MemoryEntry> {
    Ok(MemoryEntry {
        id: r.get(0)?,
        text: r.get(1)?,
        author: r.get(2)?,
        status: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}
const MEM_COLS: &str = "id, text, author, status, created_at, updated_at";

fn device_from(r: &Row) -> rusqlite::Result<DeviceRecord> {
    let pk: Vec<u8> = r.get(2)?;
    let mut public_key = [0u8; 32];
    if pk.len() == 32 {
        public_key.copy_from_slice(&pk);
    }
    Ok(DeviceRecord {
        id: r.get(0)?,
        name: r.get(1)?,
        public_key,
        created_at: r.get(3)?,
        last_seen_at: r.get(4)?,
        is_local: r.get::<_, i64>(5)? != 0,
        push_token: r.get(6)?,
        push_env: r.get(7)?,
    })
}
const DEV_COLS: &str = "id, name, public_key, created_at, last_seen_at, is_local, push_token, push_env";

#[derive(Debug, Clone)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub item_id: Option<String>,
    pub title: String,
    pub body: String,
    pub payload: String,
    /// `pending` (deferred), `sent`, `dropped`, `merged`.
    pub status: String,
    pub created_at: i64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- kv ----

    pub fn kv_get(&self, k: &str) -> Result<Option<String>> {
        Ok(self.c().query_row("SELECT v FROM kv WHERE k = ?1", [k], |r| r.get(0)).optional()?)
    }

    pub fn kv_set(&self, k: &str, v: &str) -> Result<()> {
        self.c().execute("INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v", [k, v])?;
        Ok(())
    }

    pub fn kv_delete(&self, k: &str) -> Result<()> {
        self.c().execute("DELETE FROM kv WHERE k = ?1", [k])?;
        Ok(())
    }

    pub fn get_json<T: DeserializeOwned + Default>(&self, k: &str) -> Result<T> {
        match self.kv_get(k)? {
            Some(s) => Ok(serde_json::from_str(&s).unwrap_or_default()),
            None => Ok(T::default()),
        }
    }

    pub fn set_json<T: Serialize>(&self, k: &str, v: &T) -> Result<()> {
        self.kv_set(k, &serde_json::to_string(v)?)
    }

    /// Monotonic sequence shared by messages and events.
    pub fn next_seq(&self) -> Result<i64> {
        let c = self.c();
        c.execute(
            "INSERT INTO kv (k, v) VALUES ('seq', '1') ON CONFLICT(k) DO UPDATE SET v = CAST(CAST(v AS INTEGER) + 1 AS TEXT)",
            [],
        )?;
        let v: String = c.query_row("SELECT v FROM kv WHERE k = 'seq'", [], |r| r.get(0))?;
        Ok(v.parse()?)
    }

    pub fn current_seq(&self) -> Result<i64> {
        Ok(self.kv_get("seq")?.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    pub fn profile(&self) -> Result<Profile> {
        self.get_json("profile")
    }

    pub fn settings(&self) -> Result<Settings> {
        self.get_json("settings")
    }

    // ---- messages ----

    pub fn insert_message(
        &self,
        role: &str,
        text: &str,
        item_id: Option<&str>,
        decision_id: Option<&str>,
        streaming: bool,
    ) -> Result<Message> {
        let seq = self.next_seq()?;
        let m = Message {
            id: new_id("msg"),
            seq,
            role: role.into(),
            text: text.into(),
            item_id: item_id.map(Into::into),
            decision_id: decision_id.map(Into::into),
            created_at: now_ms(),
            streaming,
        };
        self.c().execute(
            &format!("INSERT INTO messages ({MSG_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
            params![m.id, m.seq, m.role, m.text, m.item_id, m.decision_id, m.created_at, m.streaming as i64],
        )?;
        Ok(m)
    }

    /// Updates text and streaming flag; bumps the message to a new seq.
    pub fn update_message(&self, id: &str, text: &str, streaming: bool) -> Result<Message> {
        let seq = self.next_seq()?;
        self.c().execute(
            "UPDATE messages SET text = ?2, streaming = ?3, seq = ?4 WHERE id = ?1",
            params![id, text, streaming as i64, seq],
        )?;
        self.get_message(id)?.ok_or_else(|| anyhow!("message not found"))
    }

    pub fn set_message_item(&self, id: &str, item_id: &str) -> Result<()> {
        self.c().execute("UPDATE messages SET item_id = ?2 WHERE id = ?1", params![id, item_id])?;
        Ok(())
    }

    pub fn get_message(&self, id: &str) -> Result<Option<Message>> {
        Ok(self
            .c()
            .query_row(&format!("SELECT {MSG_COLS} FROM messages WHERE id = ?1"), [id], message_from)
            .optional()?)
    }

    /// The most recent `limit` messages, oldest first.
    pub fn recent_messages(&self, limit: i64) -> Result<Vec<Message>> {
        let c = self.c();
        let mut st = c.prepare(&format!(
            "SELECT {MSG_COLS} FROM (SELECT {MSG_COLS} FROM messages ORDER BY created_at DESC, seq DESC LIMIT ?1) ORDER BY created_at ASC, seq ASC"
        ))?;
        let v = st.query_map([limit], message_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    /// Marks any message left streaming by a crash as finished.
    pub fn finish_streaming_messages(&self) -> Result<()> {
        self.c().execute("UPDATE messages SET streaming = 0 WHERE streaming = 1", [])?;
        Ok(())
    }

    // ---- items ----

    pub fn save_item(&self, i: &Item) -> Result<()> {
        self.c().execute(
            &format!(
                "INSERT INTO items ({ITEM_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET title = excluded.title, detail = excluded.detail, status = excluded.status,
                 next_check_at = excluded.next_check_at, done_when = excluded.done_when, source = excluded.source,
                 checks = excluded.checks, unchanged_checks = excluded.unchanged_checks, updated_at = excluded.updated_at"
            ),
            params![
                i.id,
                i.title,
                i.detail,
                i.status,
                i.next_check_at,
                i.done_when,
                i.source,
                i.checks,
                i.unchanged_checks,
                i.created_at,
                i.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn get_item(&self, id: &str) -> Result<Option<Item>> {
        Ok(self
            .c()
            .query_row(&format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"), [id], item_from)
            .optional()?)
    }

    /// Open items plus items closed in the last 7 days.
    pub fn list_items(&self) -> Result<Vec<Item>> {
        let cutoff = now_ms() - 7 * 24 * 3600 * 1000;
        let c = self.c();
        let mut st = c.prepare(&format!(
            "SELECT {ITEM_COLS} FROM items WHERE status NOT IN ('done', 'dropped') OR updated_at > ?1 ORDER BY created_at ASC"
        ))?;
        let v = st.query_map([cutoff], item_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn items_with_status(&self, statuses: &[&str]) -> Result<Vec<Item>> {
        Ok(self.list_items()?.into_iter().filter(|i| statuses.contains(&i.status.as_str())).collect())
    }

    /// Items whose check time has come.
    pub fn due_items(&self, now: i64) -> Result<Vec<Item>> {
        let c = self.c();
        let mut st = c.prepare(&format!(
            "SELECT {ITEM_COLS} FROM items WHERE status IN ('scheduled', 'active', 'snoozed') AND next_check_at IS NOT NULL AND next_check_at <= ?1 ORDER BY next_check_at ASC"
        ))?;
        let v = st.query_map([now], item_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn next_check_time(&self) -> Result<Option<i64>> {
        Ok(self.c().query_row(
            "SELECT MIN(next_check_at) FROM items WHERE status IN ('scheduled', 'active', 'snoozed') AND next_check_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?)
    }

    /// Sources (`codex:<threadId>`) of every suggestion the user has seen, so they are not suggested twice.
    pub fn suggested_sources(&self) -> Result<Vec<String>> {
        let c = self.c();
        let mut st = c.prepare("SELECT source FROM items WHERE source LIKE 'codex:%'")?;
        let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    // ---- decisions ----

    pub fn save_decision(&self, d: &Decision) -> Result<()> {
        self.c().execute(
            &format!(
                "INSERT INTO decisions ({DEC_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET status = excluded.status, answered_at = excluded.answered_at,
                 title = excluded.title, detail = excluded.detail"
            ),
            params![d.id, d.item_id, d.title, d.detail, d.kind, d.status, d.created_at, d.expires_at, d.answered_at],
        )?;
        Ok(())
    }

    pub fn get_decision(&self, id: &str) -> Result<Option<Decision>> {
        Ok(self
            .c()
            .query_row(&format!("SELECT {DEC_COLS} FROM decisions WHERE id = ?1"), [id], decision_from)
            .optional()?)
    }

    /// Pending decisions plus those answered in the last 7 days.
    pub fn list_decisions(&self) -> Result<Vec<Decision>> {
        let cutoff = now_ms() - 7 * 24 * 3600 * 1000;
        let c = self.c();
        let mut st = c.prepare(&format!(
            "SELECT {DEC_COLS} FROM decisions WHERE status = 'pending' OR created_at > ?1 ORDER BY created_at ASC"
        ))?;
        let v = st.query_map([cutoff], decision_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn pending_decisions(&self) -> Result<Vec<Decision>> {
        Ok(self.list_decisions()?.into_iter().filter(|d| d.status == "pending").collect())
    }

    // ---- memory ----

    pub fn save_memory(&self, m: &MemoryEntry) -> Result<()> {
        self.c().execute(
            &format!(
                "INSERT INTO memory ({MEM_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET text = excluded.text, author = excluded.author,
                 status = excluded.status, updated_at = excluded.updated_at"
            ),
            params![m.id, m.text, m.author, m.status, m.created_at, m.updated_at],
        )?;
        Ok(())
    }

    pub fn get_memory(&self, id: &str) -> Result<Option<MemoryEntry>> {
        Ok(self
            .c()
            .query_row(&format!("SELECT {MEM_COLS} FROM memory WHERE id = ?1"), [id], memory_from)
            .optional()?)
    }

    /// All entries including tombstones (`deleted`).
    pub fn list_memory(&self) -> Result<Vec<MemoryEntry>> {
        let c = self.c();
        let mut st = c.prepare(&format!("SELECT {MEM_COLS} FROM memory ORDER BY created_at ASC"))?;
        let v = st.query_map([], memory_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    // ---- devices ----

    pub fn save_device(&self, d: &DeviceRecord) -> Result<()> {
        self.c().execute(
            &format!(
                "INSERT INTO devices ({DEV_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, last_seen_at = excluded.last_seen_at,
                 push_token = excluded.push_token, push_env = excluded.push_env"
            ),
            params![
                d.id,
                d.name,
                d.public_key.to_vec(),
                d.created_at,
                d.last_seen_at,
                d.is_local as i64,
                d.push_token,
                d.push_env
            ],
        )?;
        Ok(())
    }

    pub fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>> {
        Ok(self
            .c()
            .query_row(&format!("SELECT {DEV_COLS} FROM devices WHERE id = ?1"), [id], device_from)
            .optional()?)
    }

    pub fn list_devices(&self) -> Result<Vec<DeviceRecord>> {
        let c = self.c();
        let mut st = c.prepare(&format!("SELECT {DEV_COLS} FROM devices ORDER BY created_at ASC"))?;
        let v = st.query_map([], device_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn delete_device(&self, id: &str) -> Result<bool> {
        Ok(self.c().execute("DELETE FROM devices WHERE id = ?1 AND is_local = 0", [id])? > 0)
    }

    pub fn touch_device(&self, id: &str) -> Result<()> {
        self.c().execute("UPDATE devices SET last_seen_at = ?2 WHERE id = ?1", params![id, now_ms()])?;
        Ok(())
    }

    // ---- idempotency ----

    pub fn idempotent_result(&self, client_id: &str) -> Result<Option<serde_json::Value>> {
        let s: Option<String> = self
            .c()
            .query_row("SELECT result FROM idempotency WHERE client_id = ?1", [client_id], |r| r.get(0))
            .optional()?;
        Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn record_idempotent(&self, client_id: &str, result: &serde_json::Value) -> Result<()> {
        let c = self.c();
        c.execute(
            "INSERT OR REPLACE INTO idempotency (client_id, result, created_at) VALUES (?1, ?2, ?3)",
            params![client_id, result.to_string(), now_ms()],
        )?;
        c.execute("DELETE FROM idempotency WHERE created_at < ?1", [now_ms() - 30 * 24 * 3600 * 1000])?;
        Ok(())
    }

    // ---- notifications (outbox) ----

    pub fn insert_notification(&self, n: &Notification) -> Result<()> {
        self.c().execute(
            "INSERT INTO notifications (id, kind, item_id, title, body, payload, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![n.id, n.kind, n.item_id, n.title, n.body, n.payload, n.status, n.created_at],
        )?;
        Ok(())
    }

    pub fn set_notification_status(&self, id: &str, status: &str) -> Result<()> {
        let sent_at = if status == "sent" { Some(now_ms()) } else { None };
        self.c().execute(
            "UPDATE notifications SET status = ?2, sent_at = COALESCE(?3, sent_at) WHERE id = ?1",
            params![id, status, sent_at],
        )?;
        Ok(())
    }

    pub fn pending_notifications(&self) -> Result<Vec<Notification>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT id, kind, item_id, title, body, payload, status, created_at FROM notifications WHERE status = 'pending' ORDER BY created_at ASC",
        )?;
        let v = st
            .query_map([], |r| {
                Ok(Notification {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    item_id: r.get(2)?,
                    title: r.get(3)?,
                    body: r.get(4)?,
                    payload: r.get(5)?,
                    status: r.get(6)?,
                    created_at: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn sent_since(&self, since: i64, item_id: Option<&str>) -> Result<i64> {
        let c = self.c();
        Ok(match item_id {
            Some(i) => c.query_row(
                "SELECT COUNT(*) FROM notifications WHERE status = 'sent' AND sent_at >= ?1 AND item_id = ?2",
                params![since, i],
                |r| r.get(0),
            )?,
            None => c.query_row(
                "SELECT COUNT(*) FROM notifications WHERE status = 'sent' AND sent_at >= ?1",
                params![since],
                |r| r.get(0),
            )?,
        })
    }

    // ---- codex threads we created (excluded from session analysis) ----

    pub fn record_codex_thread(&self, id: &str, purpose: &str) -> Result<()> {
        self.c().execute(
            "INSERT OR IGNORE INTO codex_threads (id, purpose, created_at) VALUES (?1, ?2, ?3)",
            params![id, purpose, now_ms()],
        )?;
        Ok(())
    }

    pub fn own_codex_threads(&self, limit: i64) -> Result<Vec<String>> {
        let c = self.c();
        let mut st = c.prepare("SELECT id FROM codex_threads ORDER BY created_at DESC LIMIT ?1")?;
        let v = st.query_map([limit], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_and_messages() {
        let s = Store::open_in_memory().unwrap();
        let a = s.insert_message("user", "hi", None, None, false).unwrap();
        let b = s.insert_message("agent", "", None, None, true).unwrap();
        assert!(b.seq > a.seq);
        let b2 = s.update_message(&b.id, "hello", false).unwrap();
        assert!(b2.seq > b.seq);
        assert!(!b2.streaming);
        let all = s.recent_messages(10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].text, "hi");
        assert_eq!(s.current_seq().unwrap(), b2.seq);
    }

    #[test]
    fn settings_default_and_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let mut st = s.settings().unwrap();
        assert_eq!(st.permission_level, "workspace");
        st.paused = true;
        s.set_json("settings", &st).unwrap();
        assert!(s.settings().unwrap().paused);
    }

    #[test]
    fn idempotency() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.idempotent_result("c1").unwrap().is_none());
        s.record_idempotent("c1", &serde_json::json!({"ok": true})).unwrap();
        assert_eq!(s.idempotent_result("c1").unwrap().unwrap()["ok"], true);
    }
}
