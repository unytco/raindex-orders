use alloy::primitives::B256;
use anyhow::{Context, Result};
use holo_hash::{ActionHash, ActionHashB64};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::sync::Mutex;

/// Each withdrawal this signer signed, keyed by its nonce, with the order hash
/// it signed it for.
pub struct Records {
    conn: Mutex<Connection>,
}

impl Records {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("SIGNER_DB_PATH {path} cannot be opened"))?;
        conn.execute_batch(
            "PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS records (
                 nonce TEXT PRIMARY KEY,
                 order_hash TEXT NOT NULL,
                 withdrawal TEXT NOT NULL,
                 signed_at INTEGER NOT NULL
             )",
        )
        .with_context(|| format!("SIGNER_DB_PATH {path} cannot be opened as the records"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Records `withdrawal` under `order_hash`, unless its nonce is recorded
    /// already. Answers the other order hash it is recorded under, and then
    /// writes nothing.
    pub fn record(
        &self,
        nonce: B256,
        order_hash: B256,
        withdrawal: &ActionHash,
        signed_at: u64,
    ) -> Result<Option<B256>> {
        let mut conn = self.conn.lock().expect("records mutex poisoned");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recorded: Option<String> = tx
            .query_row(
                "SELECT order_hash FROM records WHERE nonce = ?1",
                [nonce.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        match recorded {
            Some(recorded) => {
                let recorded: B256 = recorded
                    .parse()
                    .with_context(|| format!("the record of nonce {nonce} holds no order hash"))?;
                Ok((recorded != order_hash).then_some(recorded))
            }
            None => {
                tx.execute(
                    "INSERT INTO records (nonce, order_hash, withdrawal, signed_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    (
                        nonce.to_string(),
                        order_hash.to_string(),
                        ActionHashB64::from(withdrawal.clone()).to_string(),
                        signed_at,
                    ),
                )?;
                tx.commit()?;
                Ok(None)
            }
        }
    }

    pub fn readable(&self) -> Result<()> {
        let conn = self.conn.lock().expect("records mutex poisoned");
        conn.query_row("SELECT count(*) FROM records", [], |_| Ok(()))?;
        Ok(())
    }
}
