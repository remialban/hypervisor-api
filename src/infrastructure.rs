use axum::{extract::{Path,State}, http::StatusCode, Json};

use crate::{common::{error_response, not_implemented}, models::{Disk, DiskRequest, Message, NetworkRequest},
state::SharedState};

type ApiResponse = (StatusCode, Json<Message>);

//mod hypervisors;
fn list_disks_from_libvirt(
    uri: &str,
    hypervisor_id: &str,
) -> Result<Vec<Disk>, virt::error::Error> {
    let connection = virt::connect::Connect::open(Some(uri))?;
    let pools = connection.list_all_storage_pools(0)?;
    let mut disks = Vec::new();

    for pool in pools {
        for volume in pool.list_all_volumes(0)? {
            let info = volume.get_info()?;

            disks.push(Disk {
                id: volume.get_key()?,
                hypervisor_id: hypervisor_id.to_owned(),
                name: volume.get_name()?,
                size_gb: (info.capacity / 1_073_741_824) as i64,
                format: "unknown".to_owned(),
            });
        }
    }

    Ok(disks)
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn list_disks(State(state): State<SharedState>, Path(hypervisor_id): Path<String>) -> Result<Json<Vec<Disk>>, ApiResponse> {

    // 1. Récupération de l'URI de l'hyperviseur depuis SQLite
    let (uri,) = sqlx::query_as::<_, (String,)>("SELECT uri FROM hypervisors WHERE id = ?")
        .bind(&hypervisor_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Erreur lecture BD"))?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Hyperviseur non trouvé"))?;

    // 2. Appel bloquant libvirt
    let hyp_id = hypervisor_id.clone();
    let disks = tokio::task::spawn_blocking(move || list_disks_from_libvirt(&uri, &hyp_id))
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Tâche bloquée en échec"))?
        .map_err(|_| error_response(StatusCode::BAD_GATEWAY, "Erreur libvirt"))?;

    Ok(Json(disks))
    //not_implemented("Disk listing will be connected to libvirt; disks are not stored in SQLite")
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

