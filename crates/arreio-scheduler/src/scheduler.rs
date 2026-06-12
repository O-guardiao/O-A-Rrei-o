use crate::job::{JobSchedule, JobStatus, ScheduledJob};
use anyhow::Result;
use arreio_kernel::Blackboard;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Scheduler síncrono com at-most-once semantics.
pub struct ArreioScheduler {
    blackboard: Blackboard,
}

impl ArreioScheduler {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    pub fn schedule(&self, job: ScheduledJob) -> Result<()> {
        let value = serde_json::to_value(&job)?;
        self.blackboard.put_tuple("scheduler", &job.id, value)
    }

    pub fn list(&self) -> Vec<ScheduledJob> {
        self.blackboard
            .search_tuples("scheduler", "")
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .collect()
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.blackboard.delete_tuple("scheduler", id)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<ScheduledJob> {
        self.blackboard
            .get_tuple("scheduler", id)
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Roda o loop de scheduling (bloqueante). Deve ser chamado em thread dedicada.
    pub fn run_loop<F>(&self, mut executor: F)
    where
        F: FnMut(&ScheduledJob),
    {
        loop {
            let now = now();
            for mut job in self.list() {
                if job.status == JobStatus::Pending && job.next_run <= now {
                    job.status = JobStatus::Running;
                    job.last_run = Some(now);
                    job.run_count += 1;
                    let _ = self.schedule(job.clone());
                    executor(&job);
                    // Atualiza próxima execução
                    if let JobSchedule::IntervalMinutes(m) = job.schedule {
                        job.next_run = now + (m as u64 * 60);
                        job.status = JobStatus::Pending;
                        let _ = self.schedule(job);
                    } else {
                        job.status = JobStatus::Completed;
                        let _ = self.schedule(job);
                    }
                }
            }
            thread::sleep(Duration::from_secs(10));
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_sched() -> ArreioScheduler {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        ArreioScheduler::new(bb)
    }

    #[test]
    fn schedule_and_list() {
        let sched = temp_sched();
        let job = ScheduledJob {
            id: "j1".into(),
            name: "nightly".into(),
            description: "backup".into(),
            schedule: JobSchedule::IntervalMinutes(60),
            status: JobStatus::Pending,
            command: "arreio run backup.spec".into(),
            last_run: None,
            next_run: 0,
            created_at: 0,
            run_count: 0,
        };
        sched.schedule(job).unwrap();
        assert_eq!(sched.list().len(), 1);
    }
}
