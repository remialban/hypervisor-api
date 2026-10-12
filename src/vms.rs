use axum::{extract::Path, http::StatusCode, Json};

use crate::{common::not_implemented, models::{CreateVm, Message}};

type ApiResponse = (StatusCode, Json<Message>);

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms", params(("hypervisor_id" = String, Path)), responses((status = 501, body = Message)), tag = "VMs")]
pub async fn list(Path(_hypervisor_id): Path<String>) -> ApiResponse {
    not_implemented("VM listing will be connected to libvirt; VMs are not stored in SQLite")
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 501, body = Message)), tag = "VMs")]
pub async fn get(Path((_hypervisor_id, _vm_id)): Path<(String, String)>) -> ApiResponse {
    not_implemented("VM reading will be connected to libvirt")
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
