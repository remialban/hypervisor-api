use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use roxmltree::Document;
use url::Url;
use virt::{connect::Connect, domain::Domain};

use crate::{models::VncResponse, state::SharedState};

type ApiError = (StatusCode, Json<HashMap<String, String>>);

#[utoipa::path(
    get,
    path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc",
    params(
        ("hypervisor_id" = String, Path, description = "Hôte ou IP de l'hyperviseur"),
        ("vm_id" = String, Path, description = "UUID libvirt de la VM")
    ),
    responses((status = 200, body = VncResponse)),
    tag = "VNC"
)]
pub async fn connect(
    State(state): State<SharedState>,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
) -> Result<Json<VncResponse>, ApiError> {
    let uri: String = sqlx::query_scalar("SELECT uri FROM hypervisors WHERE id = ?")
        .bind(&hypervisor_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Impossible de lire l'hyperviseur : {error}"),
            )
        })?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Hyperviseur introuvable".to_owned()))?;

    let hypervisor_host = Url::parse(&uri)
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("URI libvirt invalide : {error}"),
            )
        })?
        .host_str()
        .unwrap_or("localhost")
        .to_owned();

    // 1. Connexion à l'hyperviseur distant
    let conn =
        Connect::open(Some(&uri)).map_err(|e| api_error(StatusCode::BAD_GATEWAY, e.to_string()))?;

    // 2. Récupération du domaine par son UUID libvirt
    let domain = Domain::lookup_by_uuid_string(&conn, &vm_id)
        .map_err(|e| api_error(StatusCode::NOT_FOUND, e.to_string()))?;

    let vm_uuid = domain
        .get_uuid_string()
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let xml_desc = domain
        .get_xml_desc(0)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let doc = Document::parse(&xml_desc)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let vnc_port = doc
        .descendants()
        .find(|n| n.has_tag_name("graphics") && n.attribute("type") == Some("vnc"))
        .and_then(|n| n.attribute("port"))
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Aucun port VNC trouvé".to_owned()))?
        .to_string();

    if vnc_port == "-1" {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Le port VNC n'est pas encore attribué".to_owned(),
        ));
    }

    let token = vm_uuid.clone();
    let target = format!("{hypervisor_host}:{vnc_port}");
    update_vm_config(&token, &target).await.map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Impossible de mettre à jour vm.cfg : {error}"),
        )
    })?;

    let novnc_url = match std::env::var("NOVNC_URL") {
        Ok(base_url) => build_novnc_url(&base_url, &token),
        Err(_) => {
            let novnc_host = std::env::var("NOVNC_HOST").unwrap_or_else(|_| "localhost".to_owned());
            format!(
                "http://{novnc_host}:6080/vnc.html?path=websockify/?token={token}&autoconnect=true"
            )
        }
    };

    Ok(Json(VncResponse {
        hypervisor_id,
        vm_id,
        uuid: token.clone(),
        host: hypervisor_host,
        port: vnc_port,
        token,
        novnc_url,
        status: "connected".to_owned(),
    }))
}

async fn update_vm_config(token: &str, target: &str) -> Result<(), std::io::Error> {
    let path = std::env::var("VM_CONFIG_PATH").unwrap_or_else(|_| "vm.cfg".to_owned());
    let current = match tokio::fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };

    let entry = format!("{token}: {target}");
    let mut found = false;
    let mut lines = Vec::new();

    for line in current.lines() {
        if line.trim_start().starts_with(&format!("{token}:")) {
            lines.push(entry.clone());
            found = true;
        } else {
            lines.push(line.to_owned());
        }
    }

    if !found {
        lines.push(entry);
    }

    let mut content = lines.join("\n");
    content.push('\n');
    tokio::fs::write(path, content).await
}

fn build_novnc_url(base_url: &str, token: &str) -> String {
    let page_url = base_url.split_once('?').map_or(base_url, |(url, _)| url);
    format!("{page_url}?path=websockify/?token={token}&autoconnect=true")
}

fn api_error(status: StatusCode, message: String) -> ApiError {
    let mut values = HashMap::new();
    values.insert("status".to_owned(), "error".to_owned());
    values.insert("message".to_owned(), message);
    (status, Json(values))
}
