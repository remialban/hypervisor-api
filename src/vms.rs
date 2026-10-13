use std::sync::Arc;

use axum::{extract::{Path, Query, State}, http::{header, StatusCode}, Json};
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::{
    common::not_implemented,
    errors::ApiError,
    libvirt,
    jobs::Job,
    models::{CloneVm, CreateVm, Message, VirtualMachine},
    state::SharedState,
    vm_xml,
};

type ApiResponse = (StatusCode, Json<Message>);

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [VirtualMachine]), (status = 404, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn list(State(state): State<SharedState>, Path(hypervisor_id): Path<String>) -> Result<Json<Vec<VirtualMachine>>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    Ok(Json(libvirt::list_vms(uri, hypervisor_id).await?))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 200, body = VirtualMachine), (status = 400, body = Message), (status = 404, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn get(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>) -> Result<Json<VirtualMachine>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    Ok(Json(libvirt::get_vm(uri, hypervisor_id, vm_id).await?))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms", params(("hypervisor_id" = String, Path)), request_body = CreateVm, responses((status = 501, body = Message)), tag = "VMs")]
pub async fn create(Path(_hypervisor_id): Path<String>, Json(body): Json<CreateVm>) -> ApiResponse {
    let _ = (body.name, body.memory_mb, body.vcpus);
    not_implemented("VM creation will be connected to libvirt; VMs are not stored in SQLite")
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/start", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 200, body = VirtualMachine), (status = 400, body = Message), (status = 404, body = Message), (status = 409, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn start(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>) -> Result<Json<VirtualMachine>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    Ok(Json(libvirt::start_vm(uri, hypervisor_id, vm_id).await?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StopQuery {
    /// Cut the power instead of asking the guest to shut down (default: false).
    force: Option<bool>,
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path), StopQuery), responses((status = 200, body = VirtualMachine), (status = 400, body = Message), (status = 404, body = Message), (status = 409, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn stop(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>, Query(query): Query<StopQuery>) -> Result<Json<VirtualMachine>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let force = query.force.unwrap_or(false);
    Ok(Json(libvirt::stop_vm(uri, hypervisor_id, vm_id, force).await?))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), request_body = CloneVm, responses((status = 202, body = Job, description = "Clone started: follow it with GET /api/jobs/{job_id}"), (status = 400, body = Message), (status = 404, body = Message)), tag = "VMs")]
pub async fn clone(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>, Json(body): Json<CloneVm>) -> Result<(StatusCode, [(header::HeaderName, String); 1], Json<Job>), ApiError> {
    // Cheap checks first, so obvious mistakes fail immediately instead of inside the job.
    vm_xml::validate_name(&body.name)?;
    Uuid::parse_str(&vm_id).map_err(|_| ApiError::BadRequest("Invalid VM identifier".into()))?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;

    let job = state.jobs.start("clone");
    let job_id = job.id.clone();
    let task_state = Arc::clone(&state);
    tokio::spawn(async move {
        let outcome = libvirt::clone_vm(uri, hypervisor_id, vm_id, body.name).await;
        task_state.jobs.finish(&job_id, outcome);
    });

    let location = format!("/api/jobs/{}", job.id);
    Ok((StatusCode::ACCEPTED, [(header::LOCATION, location)], Json(job)))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteQuery {
    /// Also delete the disk volumes of the VM (default: false, only the definition is removed).
    delete_disks: Option<bool>,
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path), DeleteQuery), responses((status = 204), (status = 400, body = Message), (status = 404, body = Message), (status = 409, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn delete(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>, Query(query): Query<DeleteQuery>) -> Result<StatusCode, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    libvirt::delete_vm(uri, vm_id, query.delete_disks.unwrap_or(false)).await?;
    Ok(StatusCode::NO_CONTENT)
}
