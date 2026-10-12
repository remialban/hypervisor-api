use axum::{
    extract::Path,
    http::StatusCode,
    Json,
};

use crate::{common::non_implemente, models::{CreateVm, Message}};

#[utoipa::path(
    get,
    path = "/api/hypervisors/{hypervisor_id}/vms",
    params(("hypervisor_id" = String, Path)),
    responses((status = 501, body = Message)),
    tag = "VMs"
)]
pub async fn lister(Path(_hypervisor_id): Path<String>) -> (StatusCode, Json<Message>) {
    non_implemente("La liste des VM sera branchee a libvirt")
}

#[utoipa::path(
    get,
    path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}",
    params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)),
    responses((status = 501, body = Message)),
    tag = "VMs"
)]
pub async fn obtenir(
    Path((_hypervisor_id, _vm_id)): Path<(String, String)>,
) -> (StatusCode, Json<Message>) {
    non_implemente("La lecture d'une VM sera branchee a libvirt")
}

#[utoipa::path(
    post,
    path = "/api/hypervisors/{hypervisor_id}/vms",
    params(("hypervisor_id" = String, Path)),
    request_body = CreateVm,
    responses((status = 501, body = Message)),
    tag = "VMs"
)]
pub async fn creer(
    Path(_hypervisor_id): Path<String>,
    Json(body): Json<CreateVm>,
) -> (StatusCode, Json<Message>) {
    let _ = (body.nom, body.memoire_mb, body.vcpus);
    non_implemente("La creation de VM sera branchee a libvirt")
}

#[utoipa::path(
    post,
    path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/start",
    params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)),
    responses((status = 501, body = Message)),
    tag = "VMs"
)]
pub async fn demarrer(
    Path((_hypervisor_id, _vm_id)): Path<(String, String)>,
) -> (StatusCode, Json<Message>) {
    non_implemente("Le demarrage de VM sera branche a libvirt")
}

#[utoipa::path(
    post,
    path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop",
    params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)),
    responses((status = 501, body = Message)),
    tag = "VMs"
)]
pub async fn eteindre(
    Path((_hypervisor_id, _vm_id)): Path<(String, String)>,
) -> (StatusCode, Json<Message>) {
    non_implemente("L'arret de VM sera branche a libvirt")
}

#[utoipa::path(
    post,
    path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone",
    params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)),
    request_body = CreateVm,
    responses((status = 501, body = Message)),
    tag = "VMs"
)]
pub async fn cloner(
    Path((_hypervisor_id, _vm_id)): Path<(String, String)>,
    Json(body): Json<CreateVm>,
) -> (StatusCode, Json<Message>) {
    let _ = (body.nom, body.memoire_mb, body.vcpus);
    non_implemente("Le clonage de VM sera branche a libvirt")
}

#[utoipa::path(
    delete,
    path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}",
    params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)),
    responses((status = 501, body = Message)),
    tag = "VMs"
)]
pub async fn supprimer(
    Path((_hypervisor_id, _vm_id)): Path<(String, String)>,
) -> (StatusCode, Json<Message>) {
    non_implemente("La suppression de VM sera branchee a libvirt")
}
