use axum::{extract::Path, http::StatusCode, Json};

use crate::{common::non_implemente, models::{DiskRequest, Message, NetworkRequest}};

type ApiResponse = (StatusCode, Json<Message>);

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn disques(Path(_hypervisor_id): Path<String>) -> ApiResponse {
    non_implemente("La liste des disques sera branchee a libvirt; aucun disque n'est stocke en SQLite")
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), request_body = DiskRequest, responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn creer_disque(Path(_hypervisor_id): Path<String>, Json(body): Json<DiskRequest>) -> ApiResponse {
    let _ = (body.nom, body.taille_gb, body.format);
    non_implemente("La creation de disque sera branchee a libvirt; aucun disque n'est stocke en SQLite")
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn obtenir_disque(Path((_hypervisor_id, _disk_id)): Path<(String, String)>) -> ApiResponse {
    non_implemente("La lecture du disque sera branchee a libvirt")
}

#[utoipa::path(patch, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), request_body = DiskRequest, responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn modifier_disque(Path((_hypervisor_id, _disk_id)): Path<(String, String)>, Json(body): Json<DiskRequest>) -> ApiResponse {
    let _ = (body.nom, body.taille_gb, body.format);
    non_implemente("La modification de disque sera branchee a libvirt")
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn supprimer_disque(Path((_hypervisor_id, _disk_id)): Path<(String, String)>) -> ApiResponse {
    non_implemente("La suppression de disque sera branchee a libvirt")
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/networks", params(("hypervisor_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn reseaux(Path(_hypervisor_id): Path<String>) -> ApiResponse {
    non_implemente("La liste des reseaux sera branchee a libvirt; aucun reseau n'est stocke en SQLite")
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/networks", params(("hypervisor_id" = String, Path)), request_body = NetworkRequest, responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn creer_reseau(Path(_hypervisor_id): Path<String>, Json(body): Json<NetworkRequest>) -> ApiResponse {
    let _ = (body.nom, body.bridge);
    non_implemente("La creation de reseau sera branchee a libvirt; aucun reseau n'est stocke en SQLite")
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn vnc(Path((_hypervisor_id, _vm_id)): Path<(String, String)>) -> ApiResponse {
    non_implemente("La connexion VNC sera branchee a libvirt")
}
