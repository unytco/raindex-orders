use serde::{Deserialize, Serialize};

/// Status of a lock record
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockStatus {
    Pending,
    Confirmed,
    Processed,
    Failed,
}

impl std::fmt::Display for LockStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockStatus::Pending => write!(f, "pending"),
            LockStatus::Confirmed => write!(f, "confirmed"),
            LockStatus::Processed => write!(f, "processed"),
            LockStatus::Failed => write!(f, "failed"),
        }
    }
}

impl std::str::FromStr for LockStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(LockStatus::Pending),
            "confirmed" => Ok(LockStatus::Confirmed),
            "processed" => Ok(LockStatus::Processed),
            "failed" => Ok(LockStatus::Failed),
            _ => Err(format!("Unknown status: {}", s)),
        }
    }
}

/// A stored lock record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRecord {
    pub id: i64,
    pub lock_id: String,
    pub sender: String,
    pub amount: String,
    pub holochain_agent: String,
    pub tx_hash: String,
    pub block_number: u64,
    pub timestamp: u64,
    pub status: LockStatus,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Database statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseStats {
    pub total_locks: u64,
    pub pending_locks: u64,
    pub confirmed_locks: u64,
    pub processed_locks: u64,
    pub failed_locks: u64,
}
