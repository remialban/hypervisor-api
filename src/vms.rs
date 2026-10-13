use axum::{extract::{Path, State}, http::StatusCode, Json};

use crate::{
    common::not_implemented,
    errors::ApiError,
    libvirt,
    models::{CreateVm, Message, VirtualMachine},
    state::SharedState,
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

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/start", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 501, body = Message)), tag = "VMs")]
pub async fn start(Path((_hypervisor_id, _vm_id)): Path<(String, String)>) -> ApiResponse {
    not_implemented("VM startup will be connected to libvirt")
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 501, body = Message)), tag = "VMs")]
pub async fn stop(Path((_hypervisor_id, _vm_id)): Path<(String, String)>) -> ApiResponse {
    not_implemented("VM shutdown will be connected to libvirt")
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), request_body = CreateVm, responses((status = 501, body = Message)), tag = "VMs")]
pub async fn clone(Path((_hypervisor_id, _vm_id)): Path<(String, String)>, Json(body): Json<CreateVm>) -> ApiResponse {
    let _ = (body.name, body.memory_mb, body.vcpus);
    not_implemented("VM cloning will be connected to libvirt")
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 501, body = Message)), tag = "VMs")]
pub async fn delete(Path((_hypervisor_id, _vm_id)): Path<(String, String)>) -> ApiResponse {
    not_implemented("VM deletion will be connected to libvirt")
}
