use std::{collections::HashMap, sync::Mutex};

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    errors::ApiError,
    models::{Message, VirtualMachine},
    state::SharedState,
};

/// Finished jobs are forgotten once the registry holds this many entries.
const MAX_JOBS: usize = 200;

#[derive(Clone, Copy, PartialEq, Serialize, ToSchema)]
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
}

/// In-memory job tracking: jobs are lost when the API restarts.
#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Job>>,
}

impl JobRegistry {
    pub fn start(&self, kind: &str) -> Job {
        let job = Job {
            id: Uuid::new_v4().to_string(),
            kind: kind.to_string(),
            status: JobStatus::Running,
            result: None,
            error: None,
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

    // The lock is never held across an await and never left half-updated,
    // so a poisoned mutex is still safe to use.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[utoipa::path(get, path = "/api/jobs/{job_id}", params(("job_id" = String, Path, description = "Job identifier")), responses((status = 200, body = Job), (status = 404, body = Message)), tag = "Jobs")]
pub async fn get(State(state): State<SharedState>, Path(job_id): Path<String>) -> Result<Json<Job>, ApiError> {
    state
        .jobs
        .get(&job_id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("Job not found".into()))
}
