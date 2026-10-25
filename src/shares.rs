//! Lending a VM to someone (console + start/stop), and giving a VM to another owner.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;

use crate::{
    access::{self, Usage, VmAccess},
    audit,
    auth::{AuthUser, now},
    errors::ApiError,
    libvirt,
    models::{Message, VirtualMachine},
    state::SharedState,
};

#[derive(Serialize, ToSchema, FromRow)]
pub struct ShareView {
    pub user_id: String,
    pub username: String,
    pub created_at: i64,
    /// Unix time (seconds) when the loan ends. Null = until the owner revokes it.
    pub expires_at: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateShare {
    /// The borrower.
    pub username: String,
    /// Unix time (seconds) when the loan ends (optional).
    pub expires_at: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
pub struct ChangeOwner {
    pub owner_id: String,
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/shares", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 200, body = [ShareView]), (status = 403, body = Message), (status = 404, body = Message)), tag = "Sharing")]
pub async fn list(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
) -> Result<Json<Vec<ShareView>>, ApiError> {
    access::vm_access(&state, &user, &hypervisor_id, &vm_id)
        .await?
        .require_owner()?;
    let vm_id = access::parse_vm_id(&vm_id)?;
    let shares = sqlx::query_as::<_, ShareView>(
        "SELECT s.user_id, u.username, s.created_at, s.expires_at
         FROM vm_shares s JOIN users u ON u.id = s.user_id
         WHERE s.hypervisor_id = $1 AND s.vm_id = $2 AND (s.expires_at IS NULL OR s.expires_at > $3)
         ORDER BY u.username",
    )
    .bind(&hypervisor_id)
    .bind(vm_id)
    .bind(now())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(shares))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/shares", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), request_body = CreateShare, responses((status = 201, body = ShareView), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Sharing")]
pub async fn create(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
    Json(body): Json<CreateShare>,
) -> Result<(StatusCode, Json<ShareView>), ApiError> {
    // A borrower cannot lend the VM further: only the owner (or an admin) shares it.
    access::vm_access(&state, &user, &hypervisor_id, &vm_id)
        .await?
        .require_owner()?;
    let vm_id = access::parse_vm_id(&vm_id)?;
    if body.expires_at.is_some_and(|end| end <= now()) {
        return Err(ApiError::BadRequest(
            "expires_at must be in the future".into(),
        ));
    }

    let owner = access::owner_of(&state.db, &hypervisor_id, &vm_id)
        .await?
        .ok_or_else(|| {
            ApiError::Conflict("This VM has no owner: give it to a user before sharing it".into())
        })?;
    let borrower: Option<(String, String)> = sqlx::query_as(
        "SELECT id, username FROM users WHERE lower(username) = lower($1) AND NOT disabled",
    )
    .bind(&body.username)
    .fetch_optional(&state.db)
    .await?;
    let (borrower_id, borrower_name) =
        borrower.ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    if borrower_id == owner.0 {
        return Err(ApiError::BadRequest(
            "The owner already has full access".into(),
        ));
    }

    let created_at = now();
    sqlx::query(
        "INSERT INTO vm_shares (hypervisor_id, vm_id, user_id, created_at, expires_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (hypervisor_id, vm_id, user_id) DO UPDATE SET expires_at = excluded.expires_at",
    )
    .bind(&hypervisor_id)
    .bind(&vm_id)
    .bind(&borrower_id)
    .bind(created_at)
    .bind(body.expires_at)
    .execute(&state.db)
    .await?;
    audit::record(
        &state,
        &user,
        "vm.share",
        &format!("{hypervisor_id}/{vm_id}"),
        &borrower_name,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(ShareView {
            user_id: borrower_id,
            username: borrower_name,
            created_at,
            expires_at: body.expires_at,
        }),
    ))
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/shares/{user_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path), ("user_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message)), tag = "Sharing")]
pub async fn revoke(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id, borrower_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let vm_access = access::vm_access(&state, &user, &hypervisor_id, &vm_id).await?;
    // The owner and admins revoke anyone; a borrower can only give the VM back.
    if vm_access == VmAccess::Shared && borrower_id != user.id {
        return Err(ApiError::Forbidden(
            "Only the owner of the VM can do this".into(),
        ));
    }
    let vm_id = access::parse_vm_id(&vm_id)?;
    let result = sqlx::query(
        "DELETE FROM vm_shares WHERE hypervisor_id = $1 AND vm_id = $2 AND user_id = $3",
    )
    .bind(&hypervisor_id)
    .bind(&vm_id)
    .bind(&borrower_id)
    .execute(&state.db)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Share not found".into()));
    }
    audit::record(
        &state,
        &user,
        "vm.unshare",
        &format!("{hypervisor_id}/{vm_id}"),
        &borrower_id,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/owner", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), request_body = ChangeOwner, responses((status = 200, body = VirtualMachine), (status = 403, body = Message), (status = 404, body = Message)), tag = "Sharing")]
pub async fn change_owner(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
    Json(body): Json<ChangeOwner>,
) -> Result<Json<VirtualMachine>, ApiError> {
    user.require_admin()?;
    let vm_id = access::parse_vm_id(&vm_id)?;
    let new_owner: Option<String> =
        sqlx::query_scalar("SELECT username FROM users WHERE id = $1 AND NOT disabled")
            .bind(&body.owner_id)
            .fetch_optional(&state.db)
            .await?;
    let new_owner = new_owner.ok_or_else(|| ApiError::NotFound("User not found".into()))?;

    // The VM must really exist: this is also how VMs created outside the API get an owner.
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let vm = libvirt::get_vm(uri, hypervisor_id.clone(), vm_id.clone()).await?;

    // Keeps what the VM was created with (quota accounting); a VM first seen here has no disk size.
    access::record_owner(
        &state,
        &hypervisor_id,
        &vm_id,
        &body.owner_id,
        Usage::one_vm(vm.vcpus as u64, vm.memory_mb as u64, 0),
    )
    .await?;
    sqlx::query("DELETE FROM vm_shares WHERE hypervisor_id = $1 AND vm_id = $2 AND user_id = $3")
        .bind(&hypervisor_id)
        .bind(&vm_id)
        .bind(&body.owner_id)
        .execute(&state.db)
        .await?;
    audit::record(
        &state,
        &user,
        "vm.owner",
        &format!("{hypervisor_id}/{vm_id}"),
        &new_owner,
    )
    .await;
    Ok(Json(
        access::describe_vm(&state, &user, VmAccess::Admin, vm).await?,
    ))
}
