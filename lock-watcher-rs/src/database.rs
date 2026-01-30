use crate::types::{DatabaseStats, LockRecord, LockStatus};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// SQLite database for storing lock records
pub struct LockDatabase {
    conn: Arc<Mutex<Connection>>,
}

impl LockDatabase {
    /// Open or create the database
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)
                .context("Failed to create database directory")?;
        }

        let conn = Connection::open(path)
            .context("Failed to open database")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };

        db.init_schema()?;
        Ok(db)
    }

    /// Initialize database schema
    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "CREATE TABLE IF NOT EXISTS locks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                lock_id TEXT UNIQUE NOT NULL,
                sender TEXT NOT NULL,
                amount TEXT NOT NULL,
                holochain_agent TEXT NOT NULL,
                tx_hash TEXT NOT NULL,
                block_number INTEGER NOT NULL,
                timestamp INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                error_message TEXT,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_locks_status ON locks(status)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_locks_block ON locks(block_number)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS sync_state (
                chain_id INTEGER PRIMARY KEY,
                last_processed_block INTEGER NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )?;

        Ok(())
    }

    /// Store a new lock record
    pub fn store_lock(&self, lock: &LockRecord) -> Result<i64> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT OR IGNORE INTO locks
             (lock_id, sender, amount, holochain_agent, tx_hash, block_number, timestamp, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                lock.lock_id,
                lock.sender,
                lock.amount,
                lock.holochain_agent,
                lock.tx_hash,
                lock.block_number as i64,
                lock.timestamp as i64,
                lock.status.to_string(),
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Get a lock by lock_id
    pub fn get_lock(&self, lock_id: &str) -> Result<Option<LockRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT id, lock_id, sender, amount, holochain_agent, tx_hash,
                    block_number, timestamp, status, error_message,
                    created_at, updated_at
             FROM locks WHERE lock_id = ?1"
        )?;

        let result = stmt.query_row([lock_id], |row| {
            Ok(LockRecord {
                id: row.get(0)?,
                lock_id: row.get(1)?,
                sender: row.get(2)?,
                amount: row.get(3)?,
                holochain_agent: row.get(4)?,
                tx_hash: row.get(5)?,
                block_number: row.get::<_, i64>(6)? as u64,
                timestamp: row.get::<_, i64>(7)? as u64,
                status: row.get::<_, String>(8)?.parse().unwrap_or(LockStatus::Pending),
                error_message: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        });

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get locks by status
    pub fn get_locks_by_status(&self, status: LockStatus) -> Result<Vec<LockRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT id, lock_id, sender, amount, holochain_agent, tx_hash,
                    block_number, timestamp, status, error_message,
                    created_at, updated_at
             FROM locks WHERE status = ?1 ORDER BY block_number ASC"
        )?;

        let rows = stmt.query_map([status.to_string()], |row| {
            Ok(LockRecord {
                id: row.get(0)?,
                lock_id: row.get(1)?,
                sender: row.get(2)?,
                amount: row.get(3)?,
                holochain_agent: row.get(4)?,
                tx_hash: row.get(5)?,
                block_number: row.get::<_, i64>(6)? as u64,
                timestamp: row.get::<_, i64>(7)? as u64,
                status: row.get::<_, String>(8)?.parse().unwrap_or(LockStatus::Pending),
                error_message: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Update lock status
    pub fn update_lock_status(
        &self,
        lock_id: &str,
        status: LockStatus,
        error_message: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "UPDATE locks SET status = ?1, error_message = ?2,
             updated_at = strftime('%s', 'now') WHERE lock_id = ?3",
            params![status.to_string(), error_message, lock_id],
        )?;

        Ok(())
    }

    /// Get the last processed block for a chain
    pub fn get_last_processed_block(&self, chain_id: u64) -> Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();

        let result = conn.query_row(
            "SELECT last_processed_block FROM sync_state WHERE chain_id = ?1",
            [chain_id as i64],
            |row| row.get::<_, i64>(0),
        );

        match result {
            Ok(block) => Ok(Some(block as u64)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Set the last processed block for a chain
    pub fn set_last_processed_block(&self, chain_id: u64, block: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT OR REPLACE INTO sync_state (chain_id, last_processed_block, updated_at)
             VALUES (?1, ?2, strftime('%s', 'now'))",
            params![chain_id as i64, block as i64],
        )?;

        Ok(())
    }

    /// Get database statistics
    pub fn get_stats(&self) -> Result<DatabaseStats> {
        let conn = self.conn.lock().unwrap();

        let total: u64 = conn.query_row(
            "SELECT COUNT(*) FROM locks",
            [],
            |row| row.get(0),
        )?;

        let pending: u64 = conn.query_row(
            "SELECT COUNT(*) FROM locks WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?;

        let confirmed: u64 = conn.query_row(
            "SELECT COUNT(*) FROM locks WHERE status = 'confirmed'",
            [],
            |row| row.get(0),
        )?;

        let processed: u64 = conn.query_row(
            "SELECT COUNT(*) FROM locks WHERE status = 'processed'",
            [],
            |row| row.get(0),
        )?;

        let failed: u64 = conn.query_row(
            "SELECT COUNT(*) FROM locks WHERE status = 'failed'",
            [],
            |row| row.get(0),
        )?;

        Ok(DatabaseStats {
            total_locks: total,
            pending_locks: pending,
            confirmed_locks: confirmed,
            processed_locks: processed,
            failed_locks: failed,
        })
    }
}

impl Clone for LockDatabase {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
        }
    }
}
