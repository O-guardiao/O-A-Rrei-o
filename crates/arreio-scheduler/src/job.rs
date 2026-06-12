use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobSchedule {
    IntervalMinutes(u32),
    CronExpression(String),
    OnceAt(u64), // timestamp UNIX
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    pub description: String,
    pub schedule: JobSchedule,
    pub status: JobStatus,
    pub command: String, // comando a executar ou spec path
    pub last_run: Option<u64>,
    pub next_run: u64,
    pub created_at: u64,
    pub run_count: u32,
}
