//! Who may do what, in the spirit of Proxmox.
//!
//! * `admin` can do everything.
//! * `user` can create VMs only on the hypervisors an admin granted them
//!   (table `hypervisor_access`), and always owns the VMs they created.
//! * A VM can be lent: the borrower gets the console and start/stop, nothing else
//!   (table `vm_shares`). Access to a VM does not need access to its hypervisor.
//! * Quotas limit what a user may own (NULL = unlimited).
//!
//! libvirt knows nothing about owners: they are stored in `vm_owners`, keyed by the
//! libvirt UUID. A VM without owner (created outside this API) is reserved to admins.
//! Someone with no right on a VM gets a 404, as if it did not exist.

use std::ops::Add;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    auth::{AuthUser, now},
    errors::ApiError,
    models::VirtualMachine,
    state::SharedState,
};

/// Why the caller may touch a VM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmAccess {
    Admin,
    Owner,
    Shared,
}

impl VmAccess {
    pub fn label(self) -> &'static str {
        match self {
            VmAccess::Admin => "admin",
            VmAccess::Owner => "owner",
            VmAccess::Shared => "shared",
        }
    }

    /// Delete, clone and share are not part of what a borrower may do.
    pub fn require_owner(self) -> Result<(), ApiError> {
        match self {
            VmAccess::Shared => Err(ApiError::Forbidden(
                "Only the owner of the VM can do this".into(),
            )),
            _ => Ok(()),
        }
    }
}

/// Normalizes a VM id (a libvirt UUID) so it can be compared with what is stored.
pub fn parse_vm_id(vm_id: &str) -> Result<String, ApiError> {
    Uuid::parse_str(vm_id)
        .map(|uuid| uuid.hyphenated().to_string())
        .map_err(|_| ApiError::BadRequest("Invalid VM identifier".into()))
}

/// Checks the caller has access to the VM, and says why. 404 when they have none.
pub async fn vm_access(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    vm_id: &str,
) -> Result<VmAccess, ApiError> {
    if user.is_admin() {
        return Ok(VmAccess::Admin);
    }
    let vm_id = parse_vm_id(vm_id)?;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_id FROM vm_owners WHERE hypervisor_id = ? AND vm_id = ?")
            .bind(hypervisor_id)
            .bind(&vm_id)
            .fetch_optional(&state.db)
            .await?;
    if owner.as_deref() == Some(user.id.as_str()) {
        return Ok(VmAccess::Owner);
    }
    let shared: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM vm_shares WHERE hypervisor_id = ? AND vm_id = ? AND user_id = ?
         AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(hypervisor_id)
    .bind(&vm_id)
    .bind(&user.id)
    .bind(now())
    .fetch_optional(&state.db)
    .await?;
    if shared.is_some() {
        Ok(VmAccess::Shared)
    } else {
        Err(ApiError::NotFound("VM not found".into()))
    }
}

/// Owner (id, username) of a VM, if it has one.
pub async fn owner_of(
    db: &SqlitePool,
    hypervisor_id: &str,
    vm_id: &str,
) -> Result<Option<(String, String)>, ApiError> {
    let vm_id = parse_vm_id(vm_id)?;
    Ok(sqlx::query_as(
        "SELECT u.id, u.username FROM vm_owners o JOIN users u ON u.id = o.owner_id
         WHERE o.hypervisor_id = ? AND o.vm_id = ?",
    )
    .bind(hypervisor_id)
    .bind(vm_id)
    .fetch_optional(db)
    .await?)
}

/// Fills `owner` and `access` of a VM the caller is allowed to see.
pub async fn describe_vm(
    state: &SharedState,
    user: &AuthUser,
    access: VmAccess,
    mut vm: VirtualMachine,
) -> Result<VirtualMachine, ApiError> {
    let owner = owner_of(&state.db, &vm.hypervisor_id, &vm.id).await?;
    vm.access = Some(
        if owner.as_ref().is_some_and(|(id, _)| *id == user.id) {
            VmAccess::Owner.label()
        } else {
            access.label()
        }
        .to_owned(),
    );
    vm.owner = owner.map(|(_, username)| username);
    Ok(vm)
}

/// What the caller sees of the VMs of a hypervisor: everything for an admin,
/// only their own and the ones lent to them otherwise.
pub async fn visible_vms(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    vms: Vec<VirtualMachine>,
) -> Result<Vec<VirtualMachine>, ApiError> {
    let owners: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT o.vm_id, o.owner_id, u.username FROM vm_owners o JOIN users u ON u.id = o.owner_id
         WHERE o.hypervisor_id = ?",
    )
    .bind(hypervisor_id)
    .fetch_all(&state.db)
    .await?;
    let shared: Vec<String> = sqlx::query_scalar(
        "SELECT vm_id FROM vm_shares WHERE hypervisor_id = ? AND user_id = ?
         AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(hypervisor_id)
    .bind(&user.id)
    .bind(now())
    .fetch_all(&state.db)
    .await?;

    let mut visible = Vec::with_capacity(vms.len());
    for mut vm in vms {
        let owner = owners.iter().find(|(id, _, _)| *id == vm.id);
        let mine = owner.is_some_and(|(_, owner_id, _)| *owner_id == user.id);
        let access = if mine {
            VmAccess::Owner
        } else if user.is_admin() {
            VmAccess::Admin
        } else if shared.contains(&vm.id) {
            VmAccess::Shared
        } else {
            continue;
        };
        vm.access = Some(access.label().to_owned());
        vm.owner = owner.map(|(_, _, username)| username.clone());
        visible.push(vm);
    }
    Ok(visible)
}

pub async fn record_owner(
    state: &SharedState,
    hypervisor_id: &str,
    vm_id: &str,
    owner_id: &str,
    usage: Usage,
) -> Result<(), ApiError> {
    let vm_id = parse_vm_id(vm_id)?;
    sqlx::query(
        "INSERT INTO vm_owners (hypervisor_id, vm_id, owner_id, vcpus, memory_mb, disk_gb, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (hypervisor_id, vm_id) DO UPDATE SET owner_id = excluded.owner_id",
    )
    .bind(hypervisor_id)
    .bind(vm_id)
    .bind(owner_id)
    .bind(usage.vcpus)
    .bind(usage.memory_mb)
    .bind(usage.disk_gb)
    .bind(now())
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Forgets the owner and the shares of a deleted VM.
pub async fn forget_vm(state: &SharedState, hypervisor_id: &str, vm_id: &str) {
    let Ok(vm_id) = parse_vm_id(vm_id) else {
        return;
    };
    if let Err(error) = sqlx::query("DELETE FROM vm_owners WHERE hypervisor_id = ? AND vm_id = ?")
        .bind(hypervisor_id)
        .bind(vm_id)
        .execute(&state.db)
        .await
    {
        eprintln!("Could not forget a deleted VM: {error}");
    }
}

/// Forgets the owners of VMs that no longer exist in libvirt (deleted behind our back, for
/// example with `virsh undefine`), so they stop counting in quotas. `listed_at` is the time
/// just before the VM list was read: an owner saved after that belongs to a VM created
/// while we were listing, which the list cannot contain yet.
pub async fn prune_missing(
    state: &SharedState,
    hypervisor_id: &str,
    existing: &[VirtualMachine],
    listed_at: i64,
) {
    let known: Result<Vec<String>, _> = sqlx::query_scalar(
        "SELECT vm_id FROM vm_owners WHERE hypervisor_id = ? AND created_at < ?",
    )
    .bind(hypervisor_id)
    .bind(listed_at)
    .fetch_all(&state.db)
    .await;
    let Ok(known) = known else { return };
    for vm_id in known {
        if existing.iter().any(|vm| vm.id == vm_id) {
            continue;
        }
        if let Err(error) =
            sqlx::query("DELETE FROM vm_owners WHERE hypervisor_id = ? AND vm_id = ?")
                .bind(hypervisor_id)
                .bind(&vm_id)
                .execute(&state.db)
                .await
        {
            eprintln!("Could not forget a missing VM: {error}");
        }
    }
}

// ---------- hypervisors ----------

/// Creating VMs on a hypervisor needs a grant from an admin.
pub async fn require_grant(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
) -> Result<(), ApiError> {
    if user.is_admin() {
        return Ok(());
    }
    let granted: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM hypervisor_access WHERE user_id = ? AND hypervisor_id = ?",
    )
    .bind(&user.id)
    .bind(hypervisor_id)
    .fetch_optional(&state.db)
    .await?;
    if granted.is_some() {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "You have no access to this hypervisor".into(),
        ))
    }
}

/// Hypervisors a non-admin can see: the ones they were granted, and the ones hosting a
/// VM they own or that was lent to them.
pub const VISIBLE_HYPERVISORS: &str = "SELECT hypervisor_id FROM hypervisor_access WHERE user_id = ?1
    UNION SELECT hypervisor_id FROM vm_owners WHERE owner_id = ?1
    UNION SELECT hypervisor_id FROM vm_shares WHERE user_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)";

/// Whether the caller may see this hypervisor at all.
pub async fn hypervisor_visible(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
) -> Result<bool, ApiError> {
    if user.is_admin() {
        return Ok(true);
    }
    let found: Option<String> = sqlx::query_scalar(&format!(
        "SELECT id FROM hypervisors WHERE id = ?3 AND id IN ({VISIBLE_HYPERVISORS})"
    ))
    .bind(&user.id)
    .bind(now())
    .bind(hypervisor_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(found.is_some())
}

// ---------- quotas ----------

/// Limits on what a user can own. `null` = no limit.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct Quota {
    pub max_vms: Option<i64>,
    pub max_vcpus: Option<i64>,
    pub max_memory_mb: Option<i64>,
    pub max_disk_gb: Option<i64>,
}

/// What a user owns (or what one more VM would add).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, ToSchema)]
pub struct Usage {
    pub vms: i64,
    pub vcpus: i64,
    pub memory_mb: i64,
    pub disk_gb: i64,
}

impl Add for Usage {
    type Output = Usage;

    fn add(self, other: Usage) -> Usage {
        Usage {
            vms: self.vms + other.vms,
            vcpus: self.vcpus + other.vcpus,
            memory_mb: self.memory_mb + other.memory_mb,
            disk_gb: self.disk_gb + other.disk_gb,
        }
    }
}

impl Usage {
    /// The usage of a single VM.
    pub fn one_vm(vcpus: u64, memory_mb: u64, disk_gb: u64) -> Usage {
        Usage {
            vms: 1,
            vcpus: vcpus as i64,
            memory_mb: memory_mb as i64,
            disk_gb: disk_gb as i64,
        }
    }
}

pub async fn quota_of(db: &SqlitePool, user_id: &str) -> Result<Quota, ApiError> {
    sqlx::query_as::<_, Quota>(
        "SELECT max_vms, max_vcpus, max_memory_mb, max_disk_gb FROM users WHERE id = ?",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::NotFound("User not found".into()))
}

pub async fn usage_of(db: &SqlitePool, user_id: &str) -> Result<Usage, ApiError> {
    let (vms, vcpus, memory_mb, disk_gb): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(vcpus), 0), COALESCE(SUM(memory_mb), 0), COALESCE(SUM(disk_gb), 0)
         FROM vm_owners WHERE owner_id = ?",
    )
    .bind(user_id)
    .fetch_one(db)
    .await?;
    Ok(Usage {
        vms,
        vcpus,
        memory_mb,
        disk_gb,
    })
}

/// Refuses (403) when `extra` would push the owner over one of their limits.
/// Call it while holding `state.quota_lock`, until the new VM (or the job that will
/// create it) is registered.
pub async fn check_quota(
    state: &SharedState,
    owner_id: &str,
    extra: Usage,
) -> Result<(), ApiError> {
    let quota = quota_of(&state.db, owner_id).await?;
    let total = usage_of(&state.db, owner_id).await? + state.jobs.pending_usage(owner_id) + extra;
    let checks = [
        ("max_vms", quota.max_vms, total.vms),
        ("max_vcpus", quota.max_vcpus, total.vcpus),
        ("max_memory_mb", quota.max_memory_mb, total.memory_mb),
        ("max_disk_gb", quota.max_disk_gb, total.disk_gb),
    ];
    for (name, limit, wanted) in checks {
        if let Some(limit) = limit
            && wanted > limit
        {
            return Err(ApiError::Forbidden(format!(
                "Quota exceeded: {name} is limited to {limit} ({wanted} requested)"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_ids_are_normalized_or_rejected() {
        let upper = "6BA7B810-9DAD-11D1-80B4-00C04FD430C8";
        assert_eq!(
            parse_vm_id(upper).unwrap(),
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
        );
        assert!(matches!(parse_vm_id("nope"), Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn a_borrower_is_not_an_owner() {
        assert!(VmAccess::Owner.require_owner().is_ok());
        assert!(VmAccess::Admin.require_owner().is_ok());
        assert!(matches!(
            VmAccess::Shared.require_owner(),
            Err(ApiError::Forbidden(_))
        ));
    }

    #[test]
    fn usages_add_up() {
        let total = Usage::one_vm(2, 1024, 10) + Usage::one_vm(1, 512, 5);
        assert_eq!(
            total,
            Usage {
                vms: 2,
                vcpus: 3,
                memory_mb: 1536,
                disk_gb: 15
            }
        );
    }
}
