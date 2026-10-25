//! VM templates built with Packer, and VMs created from them.
//!
//! A template is a disk image that Packer builds for a hypervisor and that is kept in one
//! of its libvirt pools. A VM created from a template gets its own full copy of that disk.
//!
//! * Only admins create and delete templates: a Packer configuration runs arbitrary
//!   commands (provisioners, `shell-local`) on the machine that builds it.
//! * Users granted a hypervisor see its finished templates, without the Packer
//!   configuration (it often holds passwords) nor the build output, and create VMs from
//!   them within their quota. They own those VMs, as with `POST .../vms`.
//! * The build runs in the background and the row is its job, so it survives a restart:
//!   `pending` (waiting for a build slot), `in_progress`, `created` or `failed` (`error`
//!   keeps the end of the output). A build cut by a restart of the API is reported failed.
//!
//! Packer runs on the hypervisor over SSH for a `qemu+ssh://` URI (same user, port and key
//! as libvirt), on the API host for a local URI such as `qemu:///system`. That machine
//! needs `packer`, `qemu-img` and `virsh`. The built disk is converted to qcow2 if needed
//! and uploaded into the pool as `template-<name>.qcow2`.

use std::{
    collections::HashSet,
    process::Stdio,
    sync::{Arc, LazyLock, Mutex, MutexGuard},
    time::Duration,
};

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tokio::{io::AsyncWriteExt, process::Command, sync::Semaphore};
use url::Url;
use utoipa::ToSchema;
use uuid::Uuid;
use virt::{
    connect::Connect, domain::Domain, error::ErrorNumber, network::Network,
    storage_pool::StoragePool, storage_vol::StorageVol, sys,
};

use crate::{
    access::{self, Usage, VmAccess},
    audit,
    auth::{self, AuthUser, now},
    errors::ApiError,
    jobs::Job,
    libvirt,
    models::Message,
    state::SharedState,
    vm_create, vm_options, vm_xml,
};

const MAX_PACKER_CONFIG_LEN: usize = 64 * 1024;
/// A build installs a whole OS: the next ones wait as `pending`.
const MAX_CONCURRENT_BUILDS: usize = 2;
/// Default of `PACKER_TIMEOUT_SECONDS`.
const DEFAULT_BUILD_TIMEOUT: Duration = Duration::from_secs(2 * 3600);
/// How much of the build output is kept when it fails.
const MAX_ERROR_LEN: usize = 4000;
const DEFAULT_POOL: &str = "default";
const GIB: u64 = 1024 * 1024 * 1024;
const COLUMNS: &str = "id, hypervisor_id, name, packer_config, disk_name, pool, volume, status, disk_size_gb, error, created_at, updated_at";

static BUILD_SLOTS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_BUILDS);
/// Templates whose build task is alive in this process.
static RUNNING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum TemplateStatus {
    /// Waiting for a build slot.
    Pending,
    /// Packer is building the disk.
    InProgress,
    /// The disk is in its pool: VMs can be created from it.
    Created,
    Failed,
}

#[derive(Serialize, ToSchema, FromRow)]
pub struct Template {
    pub id: String,
    pub hypervisor_id: String,
    pub name: String,
    /// Admin only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packer_config: Option<String>,
    /// File name of the disk image Packer writes.
    pub disk_name: String,
    /// libvirt pool and volume holding the built disk.
    pub pool: String,
    pub volume: String,
    pub status: TemplateStatus,
    /// Virtual size of the built disk, once created.
    pub disk_size_gb: Option<i64>,
    /// End of the build output when it failed (admin only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateTemplate {
    pub name: String,
    /// Packer configuration (HCL2, or its JSON form), run on the build host.
    pub packer_config: String,
    /// File name of the disk image the configuration produces (`vm_name` of the qemu builder).
    pub disk_name: String,
    /// Pool receiving the built disk (default: `default`).
    pub pool: Option<String>,
}

/// Body of `POST .../templates/{template_id}/vms`. The disk is a copy of the template's.
#[derive(Deserialize, ToSchema)]
pub struct CreateVmFromTemplate {
    pub name: String,
    pub memory_mb: u64,
    pub vcpus: u64,
    /// Virtual network to plug the VM into (default: `default`).
    pub network: Option<String>,
    /// `bios` (default) or `uefi`: must match how the template was installed.
    pub firmware: Option<String>,
    /// Start the VM right after its creation (default: false).
    pub start: Option<bool>,
    /// Admin only: id of the user who will own the VM (default: the caller).
    pub owner_id: Option<String>,
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/templates", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [Template], description = "Admins see every template, users only the created ones"), (status = 403, body = Message), (status = 404, body = Message)), tag = "Templates")]
pub async fn list(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(hypervisor_id): Path<String>,
) -> Result<Json<Vec<Template>>, ApiError> {
    access::require_grant(&state, &user, &hypervisor_id).await?;
    libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let templates = sqlx::query_as::<_, Template>(&format!(
        "SELECT {COLUMNS} FROM templates WHERE hypervisor_id = ? ORDER BY name"
    ))
    .bind(&hypervisor_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        templates
            .into_iter()
            .filter(|template| visible(&user, template))
            .map(|template| present(&user, template))
            .collect(),
    ))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), responses((status = 200, body = Template), (status = 403, body = Message), (status = 404, body = Message)), tag = "Templates")]
pub async fn get(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
) -> Result<Json<Template>, ApiError> {
    access::require_grant(&state, &user, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let template = find(&state, &user, &hypervisor_id, &template_id).await?;
    Ok(Json(present(&user, template)))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/templates", params(("hypervisor_id" = String, Path)), request_body = CreateTemplate, responses((status = 202, body = Template, description = "Build started: follow it with GET .../templates/{template_id}"), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message, description = "Hypervisor or pool not found"), (status = 409, body = Message)), tag = "Templates")]
pub async fn create(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(hypervisor_id): Path<String>,
    Json(body): Json<CreateTemplate>,
) -> Result<
    (
        StatusCode,
        [(header::HeaderName, String); 1],
        Json<Template>,
    ),
    ApiError,
> {
    // The configuration runs commands on the build host: admins only.
    user.require_admin()?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let pool = body.pool.clone().unwrap_or_else(|| DEFAULT_POOL.into());
    validate_template(&body, &pool)?;
    let target = BuildTarget::from_uri(&uri)?;
    let volume = format!("template-{}.qcow2", body.name);
    let script = build_script(
        &body.packer_config,
        &body.disk_name,
        &pool,
        &volume,
        &target.libvirt_uri,
    )?;
    {
        let (uri, pool, volume) = (uri.clone(), pool.clone(), volume.clone());
        blocking(move || check_destination(&uri, &pool, &volume)).await?;
    }

    let id = Uuid::new_v4().to_string();
    // Registered before the row exists, so that it is never taken for an interrupted build.
    running().insert(id.clone());
    let created_at = now();
    let inserted = sqlx::query(
        "INSERT INTO templates (id, hypervisor_id, name, packer_config, disk_name, pool, volume, status, created_by, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&hypervisor_id)
    .bind(&body.name)
    .bind(&body.packer_config)
    .bind(&body.disk_name)
    .bind(&pool)
    .bind(&volume)
    .bind(&user.id)
    .bind(created_at)
    .bind(created_at)
    .execute(&state.db)
    .await;
    if let Err(error) = inserted {
        running().remove(&id);
        return Err(match error {
            sqlx::Error::Database(e) if e.is_unique_violation() => ApiError::Conflict(
                "A template with this name already exists on this hypervisor".into(),
            ),
            other => other.into(),
        });
    }
    audit::record(
        &state,
        &user,
        "template.create",
        &format!("{hypervisor_id}/{id}"),
        &body.name,
    )
    .await;

    tokio::spawn(build(
        Arc::clone(&state),
        Build {
            id: id.clone(),
            hypervisor_id: hypervisor_id.clone(),
            uri,
            target,
            script,
            pool,
            volume,
        },
    ));

    let template = find(&state, &user, &hypervisor_id, &id).await?;
    let location = format!("/api/hypervisors/{hypervisor_id}/templates/{id}");
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(present(&user, template)),
    ))
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), responses((status = 204, description = "Template and its disk deleted (VMs made from it keep their own copy)"), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Templates")]
pub async fn delete(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let template = find(&state, &user, &hypervisor_id, &template_id).await?;
    if matches!(
        template.status,
        TemplateStatus::Pending | TemplateStatus::InProgress
    ) {
        return Err(ApiError::Conflict(
            "The template is still being built".into(),
        ));
    }
    let (pool, volume) = (template.pool.clone(), template.volume.clone());
    blocking(move || delete_volume(&uri, &pool, &volume)).await?;
    sqlx::query("DELETE FROM templates WHERE id = ?")
        .bind(&template.id)
        .execute(&state.db)
        .await?;
    audit::record(
        &state,
        &user,
        "template.delete",
        &format!("{hypervisor_id}/{}", template.id),
        &template.name,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/vms", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), request_body = CreateVmFromTemplate, responses((status = 202, body = Job, description = "Creation started (the disk is copied): follow it with GET /api/jobs/{job_id}"), (status = 400, body = Message), (status = 403, body = Message, description = "No access to the hypervisor, or quota exceeded"), (status = 404, body = Message), (status = 409, body = Message)), tag = "Templates")]
pub async fn create_vm(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
    Json(body): Json<CreateVmFromTemplate>,
) -> Result<(StatusCode, [(header::HeaderName, String); 1], Json<Job>), ApiError> {
    // Same rule as POST .../vms: a grant on the hypervisor (admins have it everywhere).
    access::require_grant(&state, &user, &hypervisor_id).await?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let template = find(&state, &user, &hypervisor_id, &template_id).await?;
    if template.status != TemplateStatus::Created {
        return Err(ApiError::Conflict("The template is not built yet".into()));
    }
    let owner_id = owner_of_new_vm(&state, &user, body.owner_id.as_deref()).await?;
    validate_vm(&body)?;

    let disk_gb = template.disk_size_gb.unwrap_or(0).max(0) as u64;
    let usage = Usage::one_vm(body.vcpus, body.memory_mb, disk_gb);
    let new_vm = NewVm {
        uri: uri.clone(),
        name: body.name.clone(),
        memory_mb: body.memory_mb,
        vcpus: body.vcpus,
        uefi: body.firmware.as_deref() == Some("uefi"),
        start: body.start.unwrap_or(false),
        network: body
            .network
            .clone()
            .unwrap_or_else(|| vm_options::DEFAULT_NETWORK.into()),
        pool: template.pool.clone(),
        template_volume: template.volume.clone(),
        vnc_listen: vm_create::vnc_listen()?,
    };
    // Cheap checks first, so obvious mistakes fail at once instead of inside the job.
    let new_vm = blocking(move || check_new_vm(&new_vm).map(|_| new_vm)).await?;

    // Under the lock, so that two requests cannot both pass the check of the last free slot.
    let job = {
        let _quota = state.quota_lock.lock().await;
        access::check_quota(&state, &owner_id, usage).await?;
        state.jobs.start("template_vm", &owner_id, usage)
    };
    audit::record(
        &state,
        &user,
        "template.vm_create",
        &format!("{hypervisor_id}/{}", template.id),
        &body.name,
    )
    .await;

    let job_id = job.id.clone();
    let task_state = Arc::clone(&state);
    tokio::spawn(async move {
        let outcome = match blocking(move || define_from_template(&new_vm)).await {
            Ok(vm_id) => libvirt::get_vm(uri, hypervisor_id.clone(), vm_id).await,
            Err(error) => Err(error),
        };
        // The owner is saved before the job is declared finished: no window where the
        // client sees a VM it cannot access.
        let outcome = match outcome {
            Ok(vm) => {
                match access::record_owner(&task_state, &hypervisor_id, &vm.id, &owner_id, usage)
                    .await
                {
                    Ok(()) => {
                        let vm_access = if owner_id == user.id {
                            VmAccess::Owner
                        } else {
                            VmAccess::Admin
                        };
                        access::describe_vm(&task_state, &user, vm_access, vm).await
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        task_state.jobs.finish(&job_id, outcome);
    });

    let location = format!("/api/jobs/{}", job.id);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(job),
    ))
}

// ---------- visibility ----------

/// Users only see what they can use; admins also see the builds and their failures.
fn visible(user: &AuthUser, template: &Template) -> bool {
    user.is_admin() || template.status == TemplateStatus::Created
}

/// The configuration and the build output can hold secrets: only admins get them.
fn present(user: &AuthUser, mut template: Template) -> Template {
    if !user.is_admin() {
        template.packer_config = None;
        template.error = None;
    }
    template
}

async fn find(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    template_id: &str,
) -> Result<Template, ApiError> {
    sqlx::query_as::<_, Template>(&format!(
        "SELECT {COLUMNS} FROM templates WHERE id = ? AND hypervisor_id = ?"
    ))
    .bind(template_id)
    .bind(hypervisor_id)
    .fetch_optional(&state.db)
    .await?
    .filter(|template| visible(user, template))
    .ok_or_else(|| ApiError::NotFound("Template not found".into()))
}

/// Only an admin can create a VM for someone else.
async fn owner_of_new_vm(
    state: &SharedState,
    user: &AuthUser,
    owner_id: Option<&str>,
) -> Result<String, ApiError> {
    let Some(owner_id) = owner_id else {
        return Ok(user.id.clone());
    };
    user.require_admin()?;
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM users WHERE id = ? AND disabled = 0")
            .bind(owner_id)
            .fetch_optional(&state.db)
            .await?;
    exists.ok_or_else(|| ApiError::NotFound("Owner not found".into()))?;
    Ok(owner_id.to_owned())
}

// ---------- validation ----------

fn validate_template(body: &CreateTemplate, pool: &str) -> Result<(), ApiError> {
    // Both end up in file names and in the build script.
    vm_xml::validate_name(&body.name).map_err(|_| {
        bad("name must be 1-64 characters: letters, digits, '-', '_' or '.' (not leading)")
    })?;
    vm_xml::validate_name(&body.disk_name).map_err(|_| {
        bad("disk_name must be the file name of the disk Packer writes: 1-64 characters, letters, digits, '-', '_' or '.' (not leading)")
    })?;
    label("pool", pool)?;
    let config = &body.packer_config;
    if config.trim().is_empty() || config.len() > MAX_PACKER_CONFIG_LEN || config.contains('\0') {
        return Err(bad(&format!(
            "packer_config must be a non-empty text of at most {MAX_PACKER_CONFIG_LEN} bytes"
        )));
    }
    Ok(())
}

fn validate_vm(request: &CreateVmFromTemplate) -> Result<(), ApiError> {
    vm_xml::validate_name(&request.name)?;
    in_range(
        "memory_mb",
        request.memory_mb,
        vm_options::MIN_MEMORY_MB,
        vm_options::MAX_MEMORY_MB,
    )?;
    in_range(
        "vcpus",
        request.vcpus,
        vm_options::MIN_VCPUS,
        vm_options::MAX_VCPUS,
    )?;
    if let Some(network) = &request.network {
        label("network", network)?;
    }
    if let Some(firmware) = &request.firmware
        && !vm_options::FIRMWARES.contains(&firmware.as_str())
    {
        return Err(bad(&format!(
            "firmware must be one of {:?}",
            vm_options::FIRMWARES
        )));
    }
    Ok(())
}

fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}

fn in_range(field: &str, value: u64, min: u64, max: u64) -> Result<(), ApiError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(bad(&format!("{field} must be between {min} and {max}")))
    }
}

/// Names looked up in libvirt: printable, no path separator (a NUL byte would also make
/// the C call panic).
fn label(field: &str, value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.chars().count() > 255
        || value.chars().any(|c| c.is_control() || c == '/')
    {
        Err(bad(&format!("Invalid {field}")))
    } else {
        Ok(())
    }
}

// ---------- build ----------

#[derive(Debug, PartialEq)]
enum BuildHost {
    /// The API host itself (local libvirt URI).
    Local,
    Ssh {
        destination: String,
        port: Option<u16>,
        keyfile: Option<String>,
    },
}

/// Where Packer runs, derived from the libvirt URI of the hypervisor.
#[derive(Debug, PartialEq)]
struct BuildTarget {
    host: BuildHost,
    /// The hypervisor's libvirt URI as seen from the build host, for `virsh`.
    libvirt_uri: String,
}

impl BuildTarget {
    fn from_uri(uri: &str) -> Result<Self, ApiError> {
        let url = Url::parse(uri).map_err(|_| bad("Invalid hypervisor URI"))?;
        let (driver, transport) = url.scheme().split_once('+').unwrap_or((url.scheme(), ""));
        match (transport, url.host_str().filter(|host| !host.is_empty())) {
            ("ssh", Some(host)) => {
                let destination = match url.username() {
                    "" => host.to_owned(),
                    user => format!("{user}@{host}"),
                };
                let keyfile = url
                    .query_pairs()
                    .find(|(key, _)| key == "keyfile")
                    .map(|(_, value)| value.into_owned());
                Ok(Self {
                    host: BuildHost::Ssh {
                        destination,
                        port: url.port(),
                        keyfile,
                    },
                    libvirt_uri: format!("{driver}://{}", url.path()),
                })
            }
            (_, None) => Ok(Self {
                host: BuildHost::Local,
                libvirt_uri: uri.to_owned(),
            }),
            _ => Err(bad(
                "Templates need a local or qemu+ssh:// hypervisor: Packer is run over SSH",
            )),
        }
    }

    /// A shell reading the build script on its standard input.
    fn command(&self) -> Command {
        match &self.host {
            BuildHost::Local => {
                let mut command = Command::new("bash");
                command.arg("-s");
                command
            }
            BuildHost::Ssh {
                destination,
                port,
                keyfile,
            } => {
                let mut command = Command::new("ssh");
                command.args(["-o", "BatchMode=yes"]);
                if let Some(port) = port {
                    command.arg("-p").arg(port.to_string());
                }
                if let Some(keyfile) = keyfile {
                    command.arg("-i").arg(keyfile);
                }
                // `--`: the destination can never be read as an option.
                command.arg("--").arg(destination).arg("bash -s");
                command
            }
        }
    }
}

/// Everything the background build needs.
struct Build {
    id: String,
    hypervisor_id: String,
    uri: String,
    target: BuildTarget,
    script: String,
    pool: String,
    volume: String,
}

async fn build(state: SharedState, build: Build) {
    let outcome = run_build(&state, &build).await;
    let saved = match &outcome {
        Ok(size_gb) => {
            sqlx::query(
                "UPDATE templates SET status = 'created', disk_size_gb = ?, error = NULL, updated_at = ? WHERE id = ?",
            )
            .bind(size_gb)
            .bind(now())
            .bind(&build.id)
            .execute(&state.db)
            .await
        }
        Err(error) => {
            eprintln!("Build of template {} failed: {error}", build.id);
            sqlx::query(
                "UPDATE templates SET status = 'failed', error = ?, updated_at = ? WHERE id = ?",
            )
            .bind(error)
            .bind(now())
            .bind(&build.id)
            .execute(&state.db)
            .await
        }
    };
    if let Err(error) = saved {
        eprintln!("Could not save the state of template {}: {error}", build.id);
    }
    audit::insert(
        &state,
        None,
        None,
        "template.build",
        &format!("{}/{}", build.hypervisor_id, build.id),
        "",
        outcome.is_ok(),
    )
    .await;
    // Only once the final state is saved: see `fail_interrupted`.
    running().remove(&build.id);
}

/// Builds the disk and returns its size in GiB, or the reason of the failure.
async fn run_build(state: &SharedState, build: &Build) -> Result<i64, String> {
    let _slot = BUILD_SLOTS
        .acquire()
        .await
        .map_err(|_| "The build queue is closed".to_string())?;
    sqlx::query("UPDATE templates SET status = 'in_progress', updated_at = ? WHERE id = ?")
        .bind(now())
        .bind(&build.id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("Could not start the build: {e}"))?;
    run_script(&build.target, &build.script).await?;
    let (uri, pool, volume) = (build.uri.clone(), build.pool.clone(), build.volume.clone());
    blocking(move || register_volume(&uri, &pool, &volume))
        .await
        .map_err(|e| e.message().to_owned())
}

async fn run_script(target: &BuildTarget, script: &str) -> Result<(), String> {
    let mut child = target
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start the build: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(script.as_bytes())
        .await
        .map_err(|e| format!("Could not send the build script: {e}"))?;
    drop(stdin);
    let output = tokio::time::timeout(build_timeout(), child.wait_with_output())
        .await
        .map_err(|_| "The build timed out (PACKER_TIMEOUT_SECONDS)".to_string())?
        .map_err(|e| format!("The build could not be followed: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Err(tail(&log, MAX_ERROR_LEN))
}

fn build_timeout() -> Duration {
    std::env::var("PACKER_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_BUILD_TIMEOUT)
}

/// The script runs in a temporary directory (on disk, images are big) removed at the end.
const PREPARE: &str = r#"work=$(mktemp -d "${TMPDIR:-/var/tmp}/hypervisor-template.XXXXXX")
trap 'rm -rf "$work"' EXIT
cd "$work"
"#;

const BUILD_AND_UPLOAD: &str = r#"echo "==> packer init"
packer init .
echo "==> packer build"
packer build -color=false .
disk=$(find "$work" -type f -name "$disk_name" ! -name "$config_file" -print -quit)
if [ -z "$disk" ]; then
    echo "Packer did not produce a file named $disk_name" >&2
    exit 1
fi
if [ "$(qemu-img info "$disk" | sed -n 's/^file format: //p')" != qcow2 ]; then
    qemu-img convert -O qcow2 "$disk" "$work/converted.qcow2"
    disk="$work/converted.qcow2"
fi
echo "==> upload to $pool/$volume"
virsh -q -c "$libvirt_uri" vol-create-as "$pool" "$volume" "$(stat -c %s "$disk")" --format qcow2
if ! virsh -q -c "$libvirt_uri" vol-upload --pool "$pool" "$volume" "$disk"; then
    virsh -q -c "$libvirt_uri" vol-delete --pool "$pool" "$volume" || true
    exit 1
fi
"#;

/// The shell script sent to the build host. Every value is quoted, and the configuration
/// is passed verbatim in a quoted here-document, so nothing in it is interpreted.
fn build_script(
    config: &str,
    disk_name: &str,
    pool: &str,
    volume: &str,
    libvirt_uri: &str,
) -> Result<String, ApiError> {
    let delimiter = format!("PACKER_CONFIG_{}", auth::random_token());
    if config.contains(&delimiter) {
        return Err(bad("Invalid packer_config"));
    }
    let config_file = if config.trim_start().starts_with('{') {
        "template.pkr.json"
    } else {
        "template.pkr.hcl"
    };
    // One `{ ... }` block: bash reads all of it before running anything, so no command
    // (packer...) can swallow the rest of the script from the standard input.
    let mut script = String::from("{\nset -eu\nexec </dev/null\n");
    for (name, value) in [
        ("libvirt_uri", libvirt_uri),
        ("pool", pool),
        ("volume", volume),
        ("disk_name", disk_name),
        ("config_file", config_file),
    ] {
        script.push_str(&format!("{name}={}\n", sh_quote(value)));
    }
    script.push_str(PREPARE);
    script.push_str(&format!(
        "cat > \"$config_file\" <<'{delimiter}'\n{config}\n{delimiter}\n"
    ));
    script.push_str(BUILD_AND_UPLOAD);
    script.push_str("}\n");
    Ok(script)
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The end of `text`, where the error of a failed build is.
fn tail(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    format!(
        "…{}",
        text.chars().skip(count - max_chars).collect::<String>()
    )
}

/// Running builds are not in the database once the process is gone: a build that is
/// unfinished but has no task any more was interrupted by a restart, and never ends.
async fn fail_interrupted(state: &SharedState) -> Result<(), ApiError> {
    let unfinished: Vec<String> =
        sqlx::query_scalar("SELECT id FROM templates WHERE status IN ('pending', 'in_progress')")
            .fetch_all(&state.db)
            .await?;
    for id in unfinished {
        if running().contains(&id) {
            continue;
        }
        // The status condition: a build that just finished keeps its result.
        sqlx::query(
            "UPDATE templates SET status = 'failed', error = 'The build was interrupted by a restart of the API', updated_at = ?
             WHERE id = ? AND status IN ('pending', 'in_progress')",
        )
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

// The lock is never held across an await: a poisoned mutex is still consistent.
fn running() -> MutexGuard<'static, HashSet<String>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- libvirt (blocking) ----------

/// libvirt is blocking: run it on a dedicated thread so the async runtime is not stalled.
async fn blocking<T, F>(job: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| ApiError::Internal(format!("Blocking task failed: {e}")))?
}

fn active_pool(conn: &Connect, name: &str) -> Result<StoragePool, ApiError> {
    let pool = StoragePool::lookup_by_name(conn, name)?;
    if !pool.is_active()? {
        return Err(ApiError::Conflict(format!(
            "The storage pool {name} is not active"
        )));
    }
    Ok(pool)
}

/// Whether the pool holds a volume of that name.
fn volume_exists(pool: &StoragePool, name: &str) -> Result<bool, ApiError> {
    match StorageVol::lookup_by_name(pool, name) {
        Ok(_) => Ok(true),
        Err(e) if e.code() == ErrorNumber::NoStorageVolume => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// The built disk can be stored: the pool is active and the volume name is free.
fn check_destination(uri: &str, pool_name: &str, volume: &str) -> Result<(), ApiError> {
    let conn = Connect::open(Some(uri))?;
    let pool = active_pool(&conn, pool_name)?;
    if volume_exists(&pool, volume)? {
        return Err(ApiError::Conflict(format!(
            "A volume named {volume} already exists in pool {pool_name}"
        )));
    }
    Ok(())
}

/// Makes libvirt see the uploaded volume and returns its virtual size in GiB.
fn register_volume(uri: &str, pool: &str, volume: &str) -> Result<i64, ApiError> {
    let conn = Connect::open(Some(uri))?;
    let pool = StoragePool::lookup_by_name(&conn, pool)?;
    pool.refresh(0)?;
    let volume = StorageVol::lookup_by_name(&pool, volume)?;
    Ok(volume.get_info()?.capacity.div_ceil(GIB) as i64)
}

fn delete_volume(uri: &str, pool: &str, volume: &str) -> Result<(), ApiError> {
    let conn = Connect::open(Some(uri))?;
    let volume = match StoragePool::lookup_by_name(&conn, pool)
        .and_then(|pool| StorageVol::lookup_by_name(&pool, volume))
    {
        Ok(volume) => volume,
        // Never built, or already removed: nothing to delete.
        Err(e)
            if matches!(
                e.code(),
                ErrorNumber::NoStoragePool | ErrorNumber::NoStorageVolume
            ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    volume.delete(0).map_err(|e| match e.code() {
        ErrorNumber::OperationInvalid => ApiError::Conflict(
            "The disk of the template is in use (a VM is being created from it)".into(),
        ),
        _ => e.into(),
    })
}

/// A VM to create from a template, checked by `check_new_vm`.
struct NewVm {
    uri: String,
    name: String,
    memory_mb: u64,
    vcpus: u64,
    uefi: bool,
    start: bool,
    network: String,
    pool: String,
    template_volume: String,
    vnc_listen: String,
}

impl NewVm {
    fn disk_volume(&self) -> String {
        format!("{}.qcow2", self.name)
    }
}

/// Checks everything the creation depends on, without changing anything.
fn check_new_vm(vm: &NewVm) -> Result<(), ApiError> {
    let conn = Connect::open(Some(&vm.uri))?;
    if Domain::lookup_by_name(&conn, &vm.name).is_ok() {
        return Err(ApiError::Conflict(
            "A VM with this name already exists".into(),
        ));
    }
    if !Network::lookup_by_name(&conn, &vm.network)?.is_active()? {
        return Err(ApiError::Conflict("The network is not active".into()));
    }
    let pool = active_pool(&conn, &vm.pool)?;
    if !volume_exists(&pool, &vm.template_volume)? {
        return Err(ApiError::Conflict(
            "The disk of the template is missing from its pool".into(),
        ));
    }
    if volume_exists(&pool, &vm.disk_volume())? {
        return Err(ApiError::Conflict(format!(
            "A volume named {} already exists in pool {}",
            vm.disk_volume(),
            vm.pool
        )));
    }
    Ok(())
}

/// Copies the template disk and defines the VM on it. Slow: the whole disk is copied.
/// Nothing is left behind if a step fails. Returns the UUID of the new VM.
fn define_from_template(vm: &NewVm) -> Result<String, ApiError> {
    let conn = Connect::open(Some(&vm.uri))?;
    let pool = active_pool(&conn, &vm.pool)?;
    let source = StorageVol::lookup_by_name(&pool, &vm.template_volume)?;
    let source_xml = source.get_xml_desc(0)?;
    let format = vm_xml::volume_format(&source_xml)?;
    let volume_xml = vm_xml::new_volume_xml(&source_xml, &vm.disk_volume())?;
    let disk = StorageVol::create_xml_from(&pool, &volume_xml, &source, 0)?;
    let result = define_on_disk(&conn, vm, &disk, &format);
    if result.is_err()
        && let Err(e) = disk.delete(0)
    {
        eprintln!("Could not remove the disk of a VM that failed to be created: {e}");
    }
    result
}

fn define_on_disk(
    conn: &Connect,
    vm: &NewVm,
    disk: &StorageVol,
    format: &str,
) -> Result<String, ApiError> {
    let spec = vm_xml::DomainSpec {
        name: vm.name.clone(),
        memory_mb: vm.memory_mb,
        vcpus: vm.vcpus,
        kvm: conn.get_type()? == "QEMU",
        uefi: vm.uefi,
        disks: vec![vm_xml::DiskAttachment {
            path: disk.get_path()?,
            format: format.to_owned(),
        }],
        iso_path: None,
        network: vm.network.clone(),
        vnc_listen: vm.vnc_listen.clone(),
    };
    let domain = Domain::define_xml(conn, &vm_xml::new_domain_xml(&spec)?)?;
    if vm.start
        && let Err(e) = domain.create()
    {
        eprintln!("Could not start a VM created from a template: {e}");
        if let Err(e) = domain.undefine_flags(sys::VIR_DOMAIN_UNDEFINE_NVRAM) {
            eprintln!("Could not remove a VM that failed to start: {e}");
        }
        return Err(ApiError::Internal(
            "The VM could not be started, it was not kept".into(),
        ));
    }
    Ok(domain.get_uuid_string()?)
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::{Value, json};

    use super::*;
    use crate::http_tests::{POOL, TestApp, connect, create_volume, unique};

    // ---------- pure functions ----------

    #[test]
    fn the_build_host_follows_the_libvirt_uri() {
        assert_eq!(
            BuildTarget::from_uri("qemu:///system").unwrap(),
            BuildTarget {
                host: BuildHost::Local,
                libvirt_uri: "qemu:///system".into()
            }
        );
        assert_eq!(
            BuildTarget::from_uri("qemu+ssh://admin@kvm1:2222/system?keyfile=/keys/id").unwrap(),
            BuildTarget {
                host: BuildHost::Ssh {
                    destination: "admin@kvm1".into(),
                    port: Some(2222),
                    keyfile: Some("/keys/id".into())
                },
                libvirt_uri: "qemu:///system".into()
            }
        );
        assert_eq!(
            BuildTarget::from_uri("qemu+ssh://kvm1/session").unwrap(),
            BuildTarget {
                host: BuildHost::Ssh {
                    destination: "kvm1".into(),
                    port: None,
                    keyfile: None
                },
                libvirt_uri: "qemu:///session".into()
            }
        );
        for unsupported in ["qemu+tcp://kvm1/system", "qemu://kvm1/system", "not a uri"] {
            assert!(
                matches!(
                    BuildTarget::from_uri(unsupported),
                    Err(ApiError::BadRequest(_))
                ),
                "{unsupported}"
            );
        }
    }

    #[test]
    fn the_script_keeps_the_config_verbatim_and_quotes_the_rest() {
        let config = "source \"qemu\" \"x\" {\n  boot_command = [\"$HOME `id` '\"]\n}";
        let script =
            build_script(config, "disk.qcow2", "it's", "v.qcow2", "qemu:///system").unwrap();
        assert!(script.contains(&format!("\n{config}\n")));
        assert!(script.contains(r"pool='it'\''s'"));
        assert!(script.contains("config_file='template.pkr.hcl'"));
        assert!(script.contains("<<'PACKER_CONFIG_"));
        let json = build_script("{\"builders\": []}", "d", "p", "v", "u").unwrap();
        assert!(json.contains("config_file='template.pkr.json'"));
    }

    #[tokio::test]
    async fn the_script_is_valid_shell() {
        let script = build_script(
            "a = \"$(id)\"\n",
            "disk.qcow2",
            "p'q",
            "v",
            "qemu:///system",
        )
        .unwrap();
        let mut child = Command::new("bash")
            .arg("-n")
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(script.as_bytes()).await.unwrap();
        drop(stdin);
        let output = child.wait_with_output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_long_output_keeps_its_end() {
        assert_eq!(tail("  short  ", 10), "short");
        assert_eq!(tail("0123456789", 3), "…789");
        assert_eq!(tail("ééééé", 2), "…éé");
    }

    // ---------- HTTP ----------

    fn templates(app: &TestApp) -> String {
        format!("/api/hypervisors/{}/templates", app.hypervisor_id)
    }

    /// A user granted access to the test hypervisor.
    async fn granted_user(app: &TestApp) -> (String, String) {
        let (id, token) = app.user(&unique("user"), "user").await;
        let path = format!("/api/hypervisors/{}/access/{id}", app.hypervisor_id);
        let (status, _) = app.status_of(Method::PUT, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        (id, token)
    }

    /// A template in a given state, as if it had been built: returns its id and volume.
    async fn insert_template(app: &TestApp, status: &str) -> (String, String) {
        let name = unique("tpl");
        let volume = format!("template-{name}.qcow2");
        if status == "created" {
            create_volume(&volume);
        }
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO templates (id, hypervisor_id, name, packer_config, disk_name, pool, volume, status, disk_size_gb, created_at, updated_at)
             VALUES (?, ?, ?, 'source \"qemu\" \"x\" {}', 'disk.qcow2', ?, ?, ?, 1, 0, 0)",
        )
        .bind(&id)
        .bind(&app.hypervisor_id)
        .bind(&name)
        .bind(POOL)
        .bind(&volume)
        .bind(status)
        .execute(&app.state.db)
        .await
        .unwrap();
        (id, volume)
    }

    fn template_body(name: &str) -> Value {
        json!({
            "name": name,
            "packer_config": "this is not a valid packer configuration",
            "disk_name": "disk.qcow2",
            "pool": POOL
        })
    }

    fn volume_in_pool(name: &str) -> bool {
        let pool = StoragePool::lookup_by_name(&connect(), POOL).unwrap();
        StorageVol::lookup_by_name(&pool, name).is_ok()
    }

    #[tokio::test]
    async fn only_admins_create_and_delete_templates() {
        let app = TestApp::new().await;
        let (_, token) = granted_user(&app).await;
        let (status, _, _) = app
            .send_as(
                Some(&token),
                Method::POST,
                &templates(&app),
                Some(template_body(&unique("tpl"))),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = app
            .send_as(
                None,
                Method::POST,
                &templates(&app),
                Some(template_body(&unique("tpl"))),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (id, _) = insert_template(&app, "created").await;
        let path = format!("{}/{id}", templates(&app));
        let (status, _) = app.status_as(&token, Method::DELETE, &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn invalid_templates_are_rejected() {
        let app = TestApp::new().await;
        let mut cases = vec![];
        for (field, value) in [
            ("name", json!("../evil")),
            ("disk_name", json!("out/disk.qcow2")),
            ("packer_config", json!("   ")),
            (
                "packer_config",
                json!("x".repeat(MAX_PACKER_CONFIG_LEN + 1)),
            ),
            ("pool", json!("")),
        ] {
            let mut body = template_body(&unique("tpl"));
            body[field] = value;
            cases.push((body, StatusCode::BAD_REQUEST));
        }
        let mut missing_pool = template_body(&unique("tpl"));
        missing_pool["pool"] = json!("no-such-pool");
        cases.push((missing_pool, StatusCode::NOT_FOUND));

        for (body, expected) in cases {
            let (status, _, json) = app
                .send(Method::POST, &templates(&app), Some(body.clone()))
                .await;
            assert_eq!(status, expected, "{body} -> {json}");
        }
        let unknown = format!("/api/hypervisors/{}/templates", Uuid::new_v4());
        let (status, _, _) = app
            .send(Method::POST, &unknown, Some(template_body(&unique("tpl"))))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_build_runs_in_the_background_and_reports_its_failure() {
        let app = TestApp::new().await;
        let name = unique("tpl");
        let (status, headers, template) = app
            .send(Method::POST, &templates(&app), Some(template_body(&name)))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{template}");
        assert!(matches!(
            template["status"].as_str(),
            Some("pending" | "in_progress")
        ));
        assert_eq!(template["volume"], format!("template-{name}.qcow2"));
        let location = headers[header::LOCATION].to_str().unwrap().to_owned();
        assert_eq!(
            location,
            format!("{}/{}", templates(&app), template["id"].as_str().unwrap())
        );

        // The same name cannot be reused while the template exists.
        let (status, _, _) = app
            .send(Method::POST, &templates(&app), Some(template_body(&name)))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // An invalid configuration (or no packer at all) ends in `failed`, with the output.
        let mut last = Value::Null;
        for _ in 0..600 {
            let (status, json) = app.status_of(Method::GET, &location).await;
            assert_eq!(status, StatusCode::OK);
            last = json;
            if last["status"] == "failed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(last["status"], "failed", "{last}");
        assert!(!last["error"].as_str().unwrap_or("").is_empty());
        assert!(!volume_in_pool(&format!("template-{name}.qcow2")));
    }

    #[tokio::test]
    async fn a_build_cut_by_a_restart_is_reported_as_failed() {
        let app = TestApp::new().await;
        let (id, _) = insert_template(&app, "in_progress").await;
        let (status, json) = app
            .status_of(Method::GET, &format!("{}/{id}", templates(&app)))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "failed");
        assert!(json["error"].as_str().unwrap().contains("interrupted"));
    }

    #[tokio::test]
    async fn users_see_only_built_templates_and_never_their_configuration() {
        let app = TestApp::new().await;
        let (built, _) = insert_template(&app, "created").await;
        let (broken, _) = insert_template(&app, "failed").await;

        let (status, all) = app.status_of(Method::GET, &templates(&app)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(all.as_array().unwrap().len(), 2);
        assert!(all[0]["packer_config"].is_string());

        let (_, token) = granted_user(&app).await;
        let (status, mine) = app.status_as(&token, Method::GET, &templates(&app)).await;
        assert_eq!(status, StatusCode::OK);
        let mine = mine.as_array().unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0]["id"], built.as_str());
        assert!(mine[0].get("packer_config").is_none());
        let (status, _) = app
            .status_as(
                &token,
                Method::GET,
                &format!("{}/{broken}", templates(&app)),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // No grant on the hypervisor: no templates at all.
        let (_, stranger) = app.user(&unique("user"), "user").await;
        let (status, _) = app
            .status_as(&stranger, Method::GET, &templates(&app))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_vm_created_from_a_template_belongs_to_its_creator_and_has_its_own_disk() {
        let app = TestApp::new().await;
        let (id, template_volume) = insert_template(&app, "created").await;
        let (user_id, token) = granted_user(&app).await;
        let name = unique("vm");
        let (status, headers, job) = app
            .send_as(
                Some(&token),
                Method::POST,
                &format!("{}/{id}/vms", templates(&app)),
                Some(json!({ "name": name, "memory_mb": 128, "vcpus": 1 })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{job}");
        let job_id = job["id"].as_str().unwrap();
        assert_eq!(headers[header::LOCATION], format!("/api/jobs/{job_id}"));

        let (status, _) = app
            .status_as(&token, Method::GET, &format!("/api/jobs/{job_id}"))
            .await;
        assert_eq!(status, StatusCode::OK, "the creator follows the job");
        let job = app.wait_job(job_id).await;
        assert_eq!(job["status"], "succeeded", "{job}");
        assert_eq!(job["result"]["access"], "owner");
        let vm_id = job["result"]["id"].as_str().unwrap();

        let (status, vm) = app.status_as(&token, Method::GET, &app.vm(vm_id)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(vm["name"], name.as_str());
        let owner: String = sqlx::query_scalar("SELECT owner_id FROM vm_owners WHERE vm_id = ?")
            .bind(vm_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
        assert_eq!(owner, user_id);
        assert!(volume_in_pool(&format!("{name}.qcow2")));
        assert!(volume_in_pool(&template_volume));

        // The name is now taken: refused before any job starts.
        let (status, _, _) = app
            .send_as(
                Some(&token),
                Method::POST,
                &format!("{}/{id}/vms", templates(&app)),
                Some(json!({ "name": name, "memory_mb": 128, "vcpus": 1 })),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn creating_a_vm_from_a_template_needs_access_and_quota() {
        let app = TestApp::new().await;
        let (id, _) = insert_template(&app, "created").await;
        let path = format!("{}/{id}/vms", templates(&app));
        let body = json!({ "name": unique("vm"), "memory_mb": 128, "vcpus": 1 });

        let (_, stranger) = app.user(&unique("user"), "user").await;
        let (status, _, _) = app
            .send_as(Some(&stranger), Method::POST, &path, Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // The template disk counts in the quota of the new VM.
        let (user_id, token) = granted_user(&app).await;
        sqlx::query("UPDATE users SET max_disk_gb = 0 WHERE id = ?")
            .bind(&user_id)
            .execute(&app.state.db)
            .await
            .unwrap();
        let (status, _, json) = app
            .send_as(Some(&token), Method::POST, &path, Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(json["content"].as_str().unwrap().contains("max_disk_gb"));

        // Only an admin picks another owner.
        let mut for_someone = body.clone();
        for_someone["owner_id"] = json!(user_id);
        let (_, other) = granted_user(&app).await;
        let (status, _, _) = app
            .send_as(Some(&other), Method::POST, &path, Some(for_someone))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // A template that is not built cannot be used, even by an admin.
        let (failed, _) = insert_template(&app, "failed").await;
        let (status, _, _) = app
            .send(
                Method::POST,
                &format!("{}/{failed}/vms", templates(&app)),
                Some(body),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn deleting_a_template_removes_its_disk_but_not_while_it_builds() {
        let app = TestApp::new().await;
        let (id, volume) = insert_template(&app, "created").await;
        let path = format!("{}/{id}", templates(&app));
        let (status, _) = app.status_of(Method::DELETE, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!volume_in_pool(&volume));
        let (status, _) = app.status_of(Method::GET, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (building, _) = insert_template(&app, "in_progress").await;
        running().insert(building.clone());
        let (status, _) = app
            .status_of(Method::DELETE, &format!("{}/{building}", templates(&app)))
            .await;
        running().remove(&building);
        assert_eq!(status, StatusCode::CONFLICT);
    }
}
