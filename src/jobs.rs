use std::{collections::HashMap, sync::Mutex};

use axum::{
    Json,
    extract::{Path, State},
};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::Usage,
    auth::AuthUser,
    errors::ApiError,
    models::{Message, VirtualMachine},
    state::SharedState,
};

/// Finished jobs are forgotten once the registry holds this many entries.
const MAX_JOBS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Succeeded,
    Failed,
}

/// A long operation running in the background (a disk copy can take minutes).
#[derive(Clone, Serialize, ToSchema)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub status: JobStatus,
    /// Set when the job succeeded.
    pub result: Option<VirtualMachine>,
    /// Set when the job failed.
    pub error: Option<String>,
    /// Who asked for the job: only them (and admins) can follow it.
    #[serde(skip)]
    pub owner_id: String,
    /// What the job will add to its owner's usage: counted against the quota while running.
    #[serde(skip)]
    pub reserved: Usage,
}

/// In-memory job tracking: jobs are lost when the API restarts.
#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Job>>,
}

impl JobRegistry {
    pub fn start(&self, kind: &str, owner_id: &str, reserved: Usage) -> Job {
        let job = Job {
            id: Uuid::new_v4().to_string(),
            kind: kind.to_string(),
            status: JobStatus::Running,
            result: None,
            error: None,
            owner_id: owner_id.to_owned(),
            reserved,
        };
        let mut jobs = self.lock();
        if jobs.len() >= MAX_JOBS {
            jobs.retain(|_, j| j.status == JobStatus::Running);
        }
        jobs.insert(job.id.clone(), job.clone());
        job
    }

    pub fn finish(&self, id: &str, outcome: Result<VirtualMachine, ApiError>) {
        if let Some(job) = self.lock().get_mut(id) {
            match outcome {
                Ok(vm) => {
                    job.status = JobStatus::Succeeded;
                    job.result = Some(vm);
                }
                Err(err) => {
                    job.status = JobStatus::Failed;
                    job.error = Some(err.message().to_string());
                }
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        self.lock().get(id).cloned()
    }

    /// What the running jobs of a user are about to add to what they own.
    pub fn pending_usage(&self, owner_id: &str) -> Usage {
        self.lock()
            .values()
            .filter(|j| j.status == JobStatus::Running && j.owner_id == owner_id)
            .fold(Usage::default(), |total, j| total + j.reserved)
    }

    // The lock is never held across an await and never left half-updated,
    // so a poisoned mutex is still safe to use.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[utoipa::path(get, path = "/api/jobs/{job_id}", params(("job_id" = String, Path, description = "Job identifier")), responses((status = 200, body = Job), (status = 401, body = Message), (status = 404, body = Message)), tag = "Jobs")]
pub async fn get(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(job_id): Path<String>,
) -> Result<Json<Job>, ApiError> {
    state
        .jobs
        .get(&job_id)
        .filter(|job| user.is_admin() || job.owner_id == user.id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("Job not found".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> VirtualMachine {
        VirtualMachine {
            id: "id".into(),
            hypervisor_id: "h".into(),
            name: "vm".into(),
            memory_mb: 1,
            vcpus: 1,
            status: "stopped".into(),
            owner: None,
            access: None,
        }
    }

    #[test]
    fn a_new_job_is_running() {
        let registry = JobRegistry::default();
        let job = registry.start("clone", "u", Usage::default());
        let stored = registry.get(&job.id).unwrap();
        assert_eq!(stored.status, JobStatus::Running);
        assert!(stored.result.is_none() && stored.error.is_none());
    }

    #[test]
    fn success_keeps_the_result() {
        let registry = JobRegistry::default();
        let job = registry.start("clone", "u", Usage::default());
        registry.finish(&job.id, Ok(vm()));
        let stored = registry.get(&job.id).unwrap();
        assert_eq!(stored.status, JobStatus::Succeeded);
        assert_eq!(stored.result.unwrap().name, "vm");
    }

    #[test]
    fn failure_keeps_the_message() {
        let registry = JobRegistry::default();
        let job = registry.start("clone", "u", Usage::default());
        registry.finish(&job.id, Err(ApiError::Conflict("nope".into())));
        let stored = registry.get(&job.id).unwrap();
        assert_eq!(stored.status, JobStatus::Failed);
        assert_eq!(stored.error.as_deref(), Some("nope"));
    }

    #[test]
    fn unknown_job_is_none_and_finishing_it_is_harmless() {
        let registry = JobRegistry::default();
        registry.finish("missing", Ok(vm()));
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn full_registry_forgets_finished_jobs_but_not_running_ones() {
        let registry = JobRegistry::default();
        let running = registry.start("clone", "u", Usage::default());
        let done = registry.start("clone", "u", Usage::default());
        registry.finish(&done.id, Ok(vm()));
        for _ in 0..MAX_JOBS {
            registry.start("clone", "u", Usage::default());
        }
        assert!(registry.get(&running.id).is_some());
        assert!(registry.get(&done.id).is_none());
    }
}
