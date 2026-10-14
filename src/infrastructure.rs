use axum::{extract::{Path,State}, http::StatusCode, Json};
use std::path::Path as FilePath;
use xmltree::{Element, XMLNode};

use crate::{common::{error_response, not_implemented}, models::{Disk, DiskRequest, Message, NetworkRequest, StoragePool, StoragePoolRequest},
state::SharedState};

type ApiResponse = (StatusCode, Json<Message>);
type ApiError = (StatusCode, Json<Message>);

const DEFAULT_STORAGE_POOL: &str = "default-pool";

//mod hypervisors;
async fn get_hypervisor_uri(state: &SharedState, hypervisor_id: &str) -> Result<String, ApiError> {
    sqlx::query_as::<_, (String,)>("SELECT uri FROM hypervisors WHERE id = ?")
        .bind(hypervisor_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Erreur lecture BD"))?
        .map(|(uri,)| uri)
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Hyperviseur non trouvé"))
}

fn list_pools_from_libvirt(
    uri: &str,
    hypervisor_id: &str,
) -> Result<Vec<StoragePool>, virt::error::Error> {
    let connection = virt::connect::Connect::open(Some(uri))?;
    connection
        .list_all_storage_pools(0)?
        .iter()
        .map(|pool| {
            Ok(StoragePool {
                id: pool.get_uuid_string()?,
                hypervisor_id: hypervisor_id.to_owned(),
                name: pool.get_name()?,
                active: pool.is_active()?,
            })
        })
        .collect()
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/pools", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [StoragePool]), (status = 404, body = Message), (status = 502, body = Message)), tag = "Infrastructure")]
pub async fn list_pools(
    State(state): State<SharedState>,
    Path(hypervisor_id): Path<String>,
) -> Result<Json<Vec<StoragePool>>, ApiError> {
    let uri = get_hypervisor_uri(&state, &hypervisor_id).await?;
    let pools = tokio::task::spawn_blocking(move || {
        list_pools_from_libvirt(&uri, &hypervisor_id)
    })
    .await
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Tâche bloquée en échec"))?
    .map_err(|error| {
        eprintln!("list_pools libvirt error: {error}");
        error_response(StatusCode::BAD_GATEWAY, "Erreur libvirt")
    })?;

    Ok(Json(pools))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/pools", params(("hypervisor_id" = String, Path)), request_body = StoragePoolRequest, responses((status = 201, body = StoragePool), (status = 400, body = Message), (status = 404, body = Message), (status = 502, body = Message)), tag = "Infrastructure")]
pub async fn create_pools(
    State(state): State<SharedState>,
    Path(hypervisor_id): Path<String>,
    Json(body): Json<StoragePoolRequest>,
) -> Result<(StatusCode, Json<StoragePool>), ApiError> {
    let valid_name = !body.name.is_empty()
        && body.name.len() <= 64
        && !body.name.starts_with('.')
        && body.name.chars().all(|character| {
            character.is_ascii_alphanumeric() || "-_..".contains(character)
        });
    if !valid_name {
        return Err(error_response(StatusCode::BAD_REQUEST, "Nom de pool invalide"));
    }
    if !FilePath::new(&body.target_path).is_absolute()
        || body.target_path.chars().any(char::is_control)
    {
        return Err(error_response(StatusCode::BAD_REQUEST, "Le chemin cible doit être absolu et valide"));
    }

    let mut xml_root = Element::new("pool");
    xml_root.attributes.insert("type".to_owned(), "dir".to_owned());
    let mut name = Element::new("name");
    name.children.push(XMLNode::Text(body.name.clone()));
    let mut target = Element::new("target");
    let mut path = Element::new("path");
    path.children.push(XMLNode::Text(body.target_path));
    target.children.push(XMLNode::Element(path));
    xml_root.children.push(XMLNode::Element(name));
    xml_root.children.push(XMLNode::Element(target));
    let mut xml_bytes = Vec::new();
    xml_root.write(&mut xml_bytes).map_err(|_| {
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Erreur génération XML du pool")
    })?;
    let xml = String::from_utf8(xml_bytes).map_err(|_| {
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "XML du pool invalide")
    })?;
    let uri = get_hypervisor_uri(&state, &hypervisor_id).await?;

    let pool = tokio::task::spawn_blocking(move || {
        let connection = virt::connect::Connect::open(Some(&uri))?;
        let pool = virt::storage_pool::StoragePool::define_xml(&connection, &xml, 0)?;
        pool.build(0)?;
        pool.create(0)?;

        Ok::<StoragePool, virt::error::Error>(StoragePool {
            id: pool.get_uuid_string()?,
            hypervisor_id,
            name: pool.get_name()?,
            active: pool.is_active()?,
        })
    })
    .await
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Tâche de création en échec"))?
    .map_err(|error| {
        eprintln!("create_pools libvirt error: {error}");
        error_response(StatusCode::BAD_GATEWAY, "Erreur libvirt")
    })?;

    Ok((StatusCode::CREATED, Json(pool)))
}

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

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [Disk]), (status = 404, body = Message), (status = 500, body = Message), (status = 502, body = Message)), tag = "Infrastructure")]
pub async fn list_disks(
    State(state): State<SharedState>,
    Path(hypervisor_id): Path<String>,
) -> Result<Json<Vec<Disk>>, ApiError> {
    let uri = get_hypervisor_uri(&state, &hypervisor_id).await?;
    let disks = tokio::task::spawn_blocking(move || {
        list_disks_from_libvirt(&uri, &hypervisor_id)
    })
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Tâche bloquée en échec"))?
        .map_err(|_| error_response(StatusCode::BAD_GATEWAY, "Erreur libvirt"))?;

    Ok(Json(disks))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), request_body = DiskRequest, responses((status = 201, body = Disk), (status = 400, body = Message), (status = 404, body = Message), (status = 500, body = Message), (status = 502, body = Message)), tag = "Infrastructure")]
pub async fn create_disk(
    State(state): State<SharedState>,
    Path(hypervisor_id): Path<String>,
    Json(body): Json<DiskRequest>,
) -> Result<(StatusCode, Json<Disk>), ApiError> {
    if body.name.is_empty()
        || !body.name.chars().all(|character| {
            character.is_ascii_alphanumeric() || "-_.".contains(character)
        })
    {
        return Err(error_response(StatusCode::BAD_REQUEST, "Nom de disque invalide"));
    }
    if !matches!(body.format.as_str(), "qcow2" | "raw") {
        return Err(error_response(StatusCode::BAD_REQUEST, "Format attendu: qcow2 ou raw"));
    }

    let size_bytes = body.size_gb.checked_mul(1_073_741_824)
        .ok_or_else(|| error_response(StatusCode::BAD_REQUEST, "Taille de disque invalide"))?;
    let size_gb = i64::try_from(body.size_gb)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Taille de disque invalide"))?;
    let uri = get_hypervisor_uri(&state, &hypervisor_id).await?;

    let disk = tokio::task::spawn_blocking(move || {
        let connection = virt::connect::Connect::open(Some(&uri))?;
        let pool = virt::storage_pool::StoragePool::lookup_by_name(
            &connection,
            DEFAULT_STORAGE_POOL,
        )?;
        let xml = format!(
            "<volume><name>{}</name><capacity unit='bytes'>{size_bytes}</capacity><target><format type='{}'/></target></volume>",
            body.name,
            body.format,
        );
        let volume = virt::storage_vol::StorageVol::create_xml(&pool, &xml, 0)?;

        Ok::<Disk, virt::error::Error>(Disk {
            id: volume.get_key()?,
            hypervisor_id,
            name: body.name,
            size_gb,
            format: body.format,
        })
    })
    .await
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Tâche de création en échec"))?
    .map_err(|error| {
        eprintln!("create_disk libvirt error: {error}");
        error_response(StatusCode::BAD_GATEWAY, "Erreur libvirt")
    })?;

    Ok((StatusCode::CREATED, Json(disk)))
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

