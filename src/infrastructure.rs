use axum::{extract::Path, http::StatusCode, Json};

use crate::{common::not_implemented, models::{DiskRequest, Message, NetworkRequest}};

type ApiResponse = (StatusCode, Json<Message>);

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn list_disks(Path(_hypervisor_id): Path<String>) -> ApiResponse {
    not_implemented("Disk listing will be connected to libvirt; disks are not stored in SQLite")
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), request_body = DiskRequest, responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn create_disk(Path(_hypervisor_id): Path<String>, Json(body): Json<DiskRequest>) -> ApiResponse {
    let _ = (body.name, body.size_gb, body.format);
    not_implemented("Disk creation will be connected to libvirt; disks are not stored in SQLite")
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn get_disk(Path((_hypervisor_id, _disk_id)): Path<(String, String)>) -> ApiResponse {
    not_implemented("Disk reading will be connected to libvirt")
}

#[utoipa::path(patch, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), request_body = DiskRequest, responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn update_disk(Path((_hypervisor_id, _disk_id)): Path<(String, String)>, Json(body): Json<DiskRequest>) -> ApiResponse {
    let _ = (body.name, body.size_gb, body.format);
    not_implemented("Disk updates will be connected to libvirt")
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn delete_disk(Path((_hypervisor_id, _disk_id)): Path<(String, String)>) -> ApiResponse {
    not_implemented("Disk deletion will be connected to libvirt")
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/networks", params(("hypervisor_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn list_networks(Path(_hypervisor_id): Path<String>) -> ApiResponse {
    not_implemented("Network listing will be connected to libvirt; networks are not stored in SQLite")
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/networks", params(("hypervisor_id" = String, Path)), request_body = NetworkRequest, responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn create_network(Path(_hypervisor_id): Path<String>, Json(body): Json<NetworkRequest>) -> ApiResponse {
    let _ = (body.name, body.bridge);
    not_implemented("Network creation will be connected to libvirt; networks are not stored in SQLite")
}

