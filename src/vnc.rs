//! Console access through noVNC + websockify.
//!
//! Each call creates a random, short-lived token (`VNC_TOKEN_TTL_SECONDS`) that maps to
//! `host:port` of the VM display. websockify reads the mapping from a file generated from
//! the `vnc_tokens` table, so a console URL stops working on its own and nobody can open a
//! console by guessing a VM id. Access to the VM is checked before the token is issued.

use std::{
    path::{Path as FilePath, PathBuf},
    time::Duration,
};

use axum::{
    Json,
    extract::{Path, State},
};
use roxmltree::Document;
use url::Url;
use virt::{connect::Connect, domain::Domain};

use crate::{
    access, audit,
    auth::{self, AuthUser, now},
    errors::ApiError,
    models::{Message, VncResponse},
    state::SharedState,
};

#[utoipa::path(
    get,
    path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc",
    params(
        ("hypervisor_id" = String, Path, description = "Hypervisor identifier"),
        ("vm_id" = String, Path, description = "UUID libvirt de la VM")
    ),
    responses((status = 200, body = VncResponse), (status = 401, body = Message), (status = 404, body = Message), (status = 409, body = Message)),
    tag = "VNC"
)]
pub async fn connect(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
) -> Result<Json<VncResponse>, ApiError> {
    // Owner, borrower or admin.
    access::vm_access(&state, &user, &hypervisor_id, &vm_id).await?;
    let uri = crate::libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;

    let hypervisor_host = Url::parse(&uri)
        .map_err(|error| {
            eprintln!("Invalid libvirt URI: {error}");
            ApiError::Internal("Invalid hypervisor URI".into())
        })?
        .host_str()
        .filter(|host| !host.is_empty())
        .unwrap_or("localhost")
        .to_owned();

    let lookup_id = vm_id.clone();
    let (vm_uuid, vnc_port) = tokio::task::spawn_blocking(move || read_display(&uri, &lookup_id))
        .await
        .map_err(|e| ApiError::Internal(format!("Blocking task failed: {e}")))??;

    let token = auth::random_token();
    let expires_at = now() + state.config.vnc_ttl;
    let target = format!("{hypervisor_host}:{vnc_port}");
    sqlx::query("INSERT INTO vnc_tokens (token, target, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(&target)
        .bind(expires_at)
        .execute(&state.db)
        .await?;
    sync_token_file(&state).await.map_err(|error| {
        eprintln!("Could not write the websockify token file: {error}");
        ApiError::Internal("Could not prepare the console".into())
    })?;
    audit::record(
        &state,
        &user,
        "vm.console",
        &format!("{hypervisor_id}/{vm_uuid}"),
        "",
    )
    .await;

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
        uuid: vm_uuid,
        host: hypervisor_host,
        port: vnc_port,
        token,
        novnc_url,
        status: "connected".to_owned(),
        expires_at,
    }))
}

/// Reads the UUID and the VNC port of a VM from libvirt (blocking).
fn read_display(uri: &str, vm_id: &str) -> Result<(String, String), ApiError> {
    let conn = Connect::open(Some(uri))?;
    let domain = Domain::lookup_by_uuid_string(&conn, vm_id)?;
    let vm_uuid = domain.get_uuid_string()?;
    let xml_desc = domain.get_xml_desc(0)?;
    let doc = Document::parse(&xml_desc).map_err(|error| {
        eprintln!("Invalid domain XML: {error}");
        ApiError::Internal("Unreadable VM definition".into())
    })?;

    let port = doc
        .descendants()
        .find(|n| n.has_tag_name("graphics") && n.attribute("type") == Some("vnc"))
        .and_then(|n| n.attribute("port"))
        .ok_or_else(|| ApiError::NotFound("No VNC port found".into()))?
        .to_string();
    if port == "-1" {
        return Err(ApiError::Conflict(
            "The VNC port is not assigned yet (is the VM running?)".into(),
        ));
    }
    Ok((vm_uuid, port))
}

/// Rewrites the file read by websockify from the tokens that are still valid.
pub async fn sync_token_file(state: &SharedState) -> Result<(), std::io::Error> {
    let _guard = state.vnc_file_lock.lock().await;
    let tokens: Vec<(String, String)> = sqlx::query_as(
        "SELECT token, target FROM vnc_tokens WHERE expires_at > $1 ORDER BY expires_at",
    )
    .bind(now())
    .fetch_all(&state.db)
    .await
    .map_err(std::io::Error::other)?;
    let mut content = String::new();
    for (token, target) in tokens {
        content.push_str(&format!("{token}: {target}\n"));
    }
    write_replacing(&state.config.vnc_token_file, &content).await
}

/// Replaces the file in one step, so websockify never reads a half-written one. When the
/// file itself is a bind mount it cannot be renamed over: it is then written in place.
async fn write_replacing(path: &FilePath, content: &str) -> Result<(), std::io::Error> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    tokio::fs::write(&temporary, content).await?;
    if tokio::fs::rename(&temporary, path).await.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
        tokio::fs::write(path, content).await?;
    }
    Ok(())
}

/// How often expired tokens leave the token file: a token lives at most this much longer
/// than its lifetime.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);

/// Forgets expired tokens now and then, so they leave the token file too.
pub fn spawn_cleanup(state: SharedState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CLEANUP_INTERVAL).await;
            let removed = sqlx::query("DELETE FROM vnc_tokens WHERE expires_at <= $1")
                .bind(now())
                .execute(&state.db)
                .await
                .map(|r| r.rows_affected())
                .unwrap_or(0);
            if removed > 0
                && let Err(error) = sync_token_file(&state).await
            {
                eprintln!("Could not clean the websockify token file: {error}");
            }
        }
    });
}

fn build_novnc_url(base_url: &str, token: &str) -> String {
    let page_url = base_url.split_once('?').map_or(base_url, |(url, _)| url);
    format!("{page_url}?path=websockify/?token={token}&autoconnect=true")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_novnc_url_carries_the_token() {
        assert_eq!(
            build_novnc_url("http://host:6080/vnc.html?old=1", "abc"),
            "http://host:6080/vnc.html?path=websockify/?token=abc&autoconnect=true"
        );
    }
}
