//! HTTP-level tests of the VM routes.
//!
//! They run the real router and the real libvirt code against libvirt's in-memory
//! `test:///default` driver: no SSH, no daemon, nothing touches a real hypervisor.
//! That driver keeps one global state per process, shared by every test, so each test
//! defines its own VMs and volumes under unique names and never relies on the VM
//! that libvirt provides by default.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use virt::{connect::Connect, domain::Domain, storage_pool::StoragePool, storage_vol::StorageVol};

use crate::{auth, config::Config, routes, state::AppState};

pub(crate) const POOL: &str = "default-pool";

// ---------- HTTP helpers ----------

pub(crate) struct TestApp {
    pub(crate) router: Router,
    pub(crate) hypervisor_id: String,
    pub(crate) state: Arc<AppState>,
    /// Access token of an administrator: `send` uses it, so the VM tests below run as admin.
    pub(crate) admin_token: String,
}

impl TestApp {
    /// A fresh API with its own in-memory SQLite and one hypervisor using the test driver.
    pub(crate) async fn new() -> Self {
        let db_url = format!(
            "sqlite:file:{}?mode=memory&cache=shared",
            Uuid::new_v4().simple()
        );
        let config = Config {
            vnc_token_file: std::env::temp_dir().join(format!("vm-{}.cfg", Uuid::new_v4())),
            ..Config::default()
        };
        let state = Arc::new(
            AppState::with_config(&db_url, config)
                .await
                .expect("in-memory SQLite"),
        );
        let (_, admin_token) = insert_user(&state, "root-test", "admin").await;
        let hypervisor_id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO hypervisors (id, name, uri, status) VALUES (?, 'test', 'test:///default', 'connected')")
            .bind(&hypervisor_id)
            .execute(&state.db)
            .await
            .unwrap();
        Self {
            router: routes::create_routes(Arc::clone(&state)),
            hypervisor_id,
            state,
            admin_token,
        }
    }

    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, header::HeaderMap, Value) {
        self.send_as(Some(&self.admin_token), method, path, body)
            .await
    }

    /// Sends a request with the given access token (or none at all).
    pub(crate) async fn send_as(
        &self,
        token: Option<&str>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, header::HeaderMap, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some(json) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (parts.status, parts.headers, json)
    }

    pub(crate) fn vms(&self) -> String {
        format!("/api/hypervisors/{}/vms", self.hypervisor_id)
    }

    pub(crate) fn vm(&self, vm_id: &str) -> String {
        format!("{}/{vm_id}", self.vms())
    }

    pub(crate) async fn status_of(&self, method: Method, path: &str) -> (StatusCode, Value) {
        let (status, _, json) = self.send(method, path, None).await;
        (status, json)
    }

    /// Like `status_of`, as the holder of `token`.
    pub(crate) async fn status_as(
        &self,
        token: &str,
        method: Method,
        path: &str,
    ) -> (StatusCode, Value) {
        let (status, _, json) = self.send_as(Some(token), method, path, None).await;
        (status, json)
    }

    /// Creates an account (with a made-up password hash) and returns its id and access token.
    pub(crate) async fn user(&self, username: &str, role: &str) -> (String, String) {
        insert_user(&self.state, username, role).await
    }

    /// Waits for a job to leave the `running` state and returns it.
    pub(crate) async fn wait_job(&self, job_id: &str) -> Value {
        for _ in 0..200 {
            let (status, job) = self
                .status_of(Method::GET, &format!("/api/jobs/{job_id}"))
                .await;
            assert_eq!(status, StatusCode::OK);
            if job["status"] != "running" {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("job {job_id} did not finish");
    }

    /// Asks for a clone and waits for the job to finish.
    pub(crate) async fn clone_vm(&self, vm_id: &str, name: &str) -> Value {
        let (status, _, job) = self
            .send(
                Method::POST,
                &format!("{}/clone", self.vm(vm_id)),
                Some(json!({ "name": name })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        self.wait_job(job["id"].as_str().unwrap()).await
    }
}

async fn insert_user(state: &Arc<AppState>, username: &str, role: &str) -> (String, String) {
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, ?, 'x', ?, 0)")
        .bind(&id)
        .bind(username)
        .bind(role)
        .execute(&state.db)
        .await
        .unwrap();
    let token = auth::issue_access_token(state, &id).unwrap();
    (id, token)
}

// ---------- libvirt fixtures (direct calls on the test driver) ----------

pub(crate) fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().simple().to_string()[..8])
}

fn connect() -> Connect {
    Connect::open(Some("test:///default")).unwrap()
}

/// Creates a volume in the test pool and returns its path.
pub(crate) fn create_volume(name: &str) -> String {
    let conn = connect();
    let pool = StoragePool::lookup_by_name(&conn, POOL).unwrap();
    let xml = format!(
        "<volume><name>{name}</name><capacity unit='MiB'>1</capacity><target><format type='raw'/></target></volume>"
    );
    StorageVol::create_xml(&pool, &xml, 0)
        .unwrap()
        .get_path()
        .unwrap()
}

fn volume_exists(path: &str) -> bool {
    StorageVol::lookup_by_path(&connect(), path).is_ok()
}

/// Defines a stopped VM with the given disk files and returns its UUID.
pub(crate) fn define_vm(name: &str, disks: &[&str]) -> String {
    let disks: String = disks
        .iter()
        .enumerate()
        .map(|(i, path)| format!("<disk type='file' device='disk'><source file='{path}'/><target dev='vd{}'/></disk>", (b'a' + i as u8) as char))
        .collect();
    let xml = format!(
        "<domain type='test'><name>{name}</name><memory unit='MiB'>64</memory><vcpu>1</vcpu><os><type>hvm</type></os><devices>{disks}</devices></domain>"
    );
    let conn = connect();
    Domain::define_xml(&conn, &xml)
        .unwrap()
        .get_uuid_string()
        .unwrap()
}

// ---------- list / get ----------

#[tokio::test]
async fn list_contains_a_defined_vm() {
    let app = TestApp::new().await;
    let name = unique("list");
    let id = define_vm(&name, &[]);

    let (status, vms) = app.status_of(Method::GET, &app.vms()).await;

    assert_eq!(status, StatusCode::OK);
    let vm = vms
        .as_array()
        .unwrap()
        .iter()
        .find(|vm| vm["id"] == id)
        .expect("VM listed");
    assert_eq!(vm["name"], name);
    assert_eq!(vm["status"], "stopped");
    assert_eq!(vm["memory_mb"], 64);
    assert_eq!(vm["vcpus"], 1);
    assert_eq!(vm["hypervisor_id"], app.hypervisor_id);
}

#[tokio::test]
async fn get_returns_one_vm() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("get"), &[]);

    let (status, vm) = app.status_of(Method::GET, &app.vm(&id)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(vm["id"], id);
}

#[tokio::test]
async fn get_rejects_a_malformed_vm_id() {
    let app = TestApp::new().await;
    let (status, body) = app.status_of(Method::GET, &app.vm("not-a-uuid")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], "error");
}

#[tokio::test]
async fn get_unknown_vm_is_404() {
    let app = TestApp::new().await;
    let (status, _) = app
        .status_of(Method::GET, &app.vm(&Uuid::new_v4().to_string()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_hypervisor_is_404_on_every_vm_route() {
    let app = TestApp::new().await;
    let vm = Uuid::new_v4();
    let base = "/api/hypervisors/unknown/vms";
    for (method, path) in [
        (Method::GET, base.to_string()),
        (Method::GET, format!("{base}/{vm}")),
        (Method::POST, format!("{base}/{vm}/start")),
        (Method::POST, format!("{base}/{vm}/stop")),
        (Method::DELETE, format!("{base}/{vm}")),
    ] {
        let (status, body) = app.status_of(method.clone(), &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(body["content"], "Hypervisor not found");
    }
}

// ---------- start / stop ----------

#[tokio::test]
async fn start_then_stop_a_vm() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("power"), &[]);

    let (status, vm) = app
        .status_of(Method::POST, &format!("{}/start", app.vm(&id)))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(vm["status"], "running");

    let (status, vm) = app
        .status_of(Method::POST, &format!("{}/stop?force=true", app.vm(&id)))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(vm["status"], "stopped");

    let (_, vm) = app.status_of(Method::GET, &app.vm(&id)).await;
    assert_eq!(vm["status"], "stopped");
}

#[tokio::test]
async fn graceful_stop_is_accepted_on_a_running_vm() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("graceful"), &[]);
    app.status_of(Method::POST, &format!("{}/start", app.vm(&id)))
        .await;

    let (status, _) = app
        .status_of(Method::POST, &format!("{}/stop", app.vm(&id)))
        .await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn starting_a_running_vm_is_a_conflict() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("twice"), &[]);
    let start = format!("{}/start", app.vm(&id));
    app.status_of(Method::POST, &start).await;

    let (status, body) = app.status_of(Method::POST, &start).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["status"], "error");
}

#[tokio::test]
async fn stopping_a_stopped_vm_is_a_conflict() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("stopped"), &[]);
    for suffix in ["stop", "stop?force=true"] {
        let (status, _) = app
            .status_of(Method::POST, &format!("{}/{suffix}", app.vm(&id)))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{suffix}");
    }
}

#[tokio::test]
async fn power_actions_on_unknown_or_malformed_ids() {
    let app = TestApp::new().await;
    let unknown = Uuid::new_v4().to_string();
    for action in ["start", "stop"] {
        let (status, _) = app
            .status_of(Method::POST, &format!("{}/{action}", app.vm(&unknown)))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{action} unknown");
        let (status, _) = app
            .status_of(Method::POST, &format!("{}/{action}", app.vm("nope")))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{action} malformed");
    }
}

#[tokio::test]
async fn stop_rejects_a_bad_force_value() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("badforce"), &[]);
    let (status, _) = app
        .status_of(Method::POST, &format!("{}/stop?force=maybe", app.vm(&id)))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------- delete ----------

#[tokio::test]
async fn delete_removes_the_vm_but_keeps_its_disk_by_default() {
    let app = TestApp::new().await;
    let disk = create_volume(&format!("{}.img", unique("keep")));
    let id = define_vm(&unique("del"), &[&disk]);

    let (status, _) = app.status_of(Method::DELETE, &app.vm(&id)).await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.status_of(Method::GET, &app.vm(&id)).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(volume_exists(&disk), "the disk must be kept");
}

#[tokio::test]
async fn delete_with_delete_disks_removes_the_disk_too() {
    let app = TestApp::new().await;
    let disk = create_volume(&format!("{}.img", unique("gone")));
    let id = define_vm(&unique("deldisk"), &[&disk]);

    let (status, _) = app
        .status_of(
            Method::DELETE,
            &format!("{}?delete_disks=true", app.vm(&id)),
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!volume_exists(&disk), "the disk must be deleted");
}

#[tokio::test]
async fn delete_of_a_running_vm_is_refused() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("busy"), &[]);
    app.status_of(Method::POST, &format!("{}/start", app.vm(&id)))
        .await;

    let (status, _) = app.status_of(Method::DELETE, &app.vm(&id)).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        app.status_of(Method::GET, &app.vm(&id)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn delete_disks_is_refused_when_another_vm_uses_the_disk() {
    let app = TestApp::new().await;
    let disk = create_volume(&format!("{}.img", unique("shared")));
    let first = define_vm(&unique("owner1"), &[&disk]);
    let second = define_vm(&unique("owner2"), &[&disk]);

    let (status, _) = app
        .status_of(
            Method::DELETE,
            &format!("{}?delete_disks=true", app.vm(&first)),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        app.status_of(Method::GET, &app.vm(&first)).await.0,
        StatusCode::OK,
        "nothing deleted"
    );
    assert!(volume_exists(&disk));
    // Without delete_disks the same deletion is fine, and the other VM keeps its disk.
    assert_eq!(
        app.status_of(Method::DELETE, &app.vm(&first)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.status_of(Method::GET, &app.vm(&second)).await.0,
        StatusCode::OK
    );
    assert!(volume_exists(&disk));
}

#[tokio::test]
async fn delete_disks_is_refused_for_a_disk_outside_any_pool() {
    let app = TestApp::new().await;
    let in_pool = create_volume(&format!("{}.img", unique("pooled")));
    let id = define_vm(&unique("outside"), &[&in_pool, "/nowhere/disk.img"]);

    let (status, _) = app
        .status_of(
            Method::DELETE,
            &format!("{}?delete_disks=true", app.vm(&id)),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        app.status_of(Method::GET, &app.vm(&id)).await.0,
        StatusCode::OK
    );
    assert!(
        volume_exists(&in_pool),
        "the pooled disk must not be deleted either"
    );
}

#[tokio::test]
async fn delete_unknown_vm_is_404() {
    let app = TestApp::new().await;
    let (status, _) = app
        .status_of(Method::DELETE, &app.vm(&Uuid::new_v4().to_string()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------- clone (asynchronous) ----------

#[tokio::test]
async fn clone_answers_202_with_a_location_and_a_job() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("src"), &[]);

    let (status, headers, job) = app
        .send(
            Method::POST,
            &format!("{}/clone", app.vm(&id)),
            Some(json!({ "name": unique("c") })),
        )
        .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(job["kind"], "clone");
    let job_id = job["id"].as_str().unwrap();
    assert_eq!(
        headers[header::LOCATION],
        format!("/api/jobs/{job_id}").as_str()
    );
    app.wait_job(job_id).await;
}

#[tokio::test]
async fn clone_copies_the_vm_and_its_disk() {
    let app = TestApp::new().await;
    let disk = create_volume(&format!("{}.img", unique("srcdisk")));
    let source = define_vm(&unique("src"), &[&disk]);
    let name = unique("clone");

    let job = app.clone_vm(&source, &name).await;

    assert_eq!(job["status"], "succeeded", "{job}");
    let clone = &job["result"];
    assert_eq!(clone["name"], name);
    assert_eq!(clone["status"], "stopped");
    assert_ne!(clone["id"], source, "the clone has its own UUID");
    // The clone is a real VM, with its own disk copy next to the original one.
    assert_eq!(
        app.status_of(Method::GET, &app.vm(clone["id"].as_str().unwrap()))
            .await
            .0,
        StatusCode::OK
    );
    let copy = format!("/{POOL}/{name}.qcow2");
    assert!(volume_exists(&copy));
    assert!(volume_exists(&disk), "the source disk is untouched");

    // And deleting the clone with its disks leaves the source alone.
    let clone_id = clone["id"].as_str().unwrap();
    let (status, _) = app
        .status_of(
            Method::DELETE,
            &format!("{}?delete_disks=true", app.vm(clone_id)),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!volume_exists(&copy));
    assert!(volume_exists(&disk));
    assert_eq!(
        app.status_of(Method::GET, &app.vm(&source)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn clone_of_a_running_vm_fails() {
    let app = TestApp::new().await;
    let source = define_vm(&unique("running"), &[]);
    app.status_of(Method::POST, &format!("{}/start", app.vm(&source)))
        .await;
    let name = unique("clone");

    let job = app.clone_vm(&source, &name).await;

    assert_eq!(job["status"], "failed");
    assert_eq!(job["error"], "The source VM must be stopped before cloning");
    assert!(job["result"].is_null());
}

#[tokio::test]
async fn clone_with_an_existing_name_fails() {
    let app = TestApp::new().await;
    let source = define_vm(&unique("src"), &[]);
    let taken = unique("taken");
    define_vm(&taken, &[]);

    let job = app.clone_vm(&source, &taken).await;

    assert_eq!(job["status"], "failed");
    assert_eq!(job["error"], "A VM with this name already exists");
}

#[tokio::test]
async fn failed_clone_removes_the_disks_it_already_copied() {
    let app = TestApp::new().await;
    let pooled = create_volume(&format!("{}.img", unique("pooled")));
    // The second disk cannot be copied: the first copy must be rolled back.
    let source = define_vm(&unique("src"), &[&pooled, "/nowhere/disk.img"]);
    let name = unique("clone");

    let job = app.clone_vm(&source, &name).await;

    assert_eq!(job["status"], "failed");
    assert!(
        !volume_exists(&format!("/{POOL}/{name}.qcow2")),
        "partial copy left behind"
    );
    let (_, vms) = app.status_of(Method::GET, &app.vms()).await;
    assert!(
        vms.as_array().unwrap().iter().all(|vm| vm["name"] != name),
        "no VM was defined"
    );
}

#[tokio::test]
async fn clone_rejects_bad_requests_immediately() {
    let app = TestApp::new().await;
    let id = define_vm(&unique("src"), &[]);
    let clone = |vm: &str| format!("{}/clone", app.vm(vm));

    for name in ["../evil", "", "with space", ".hidden"] {
        let (status, _, _) = app
            .send(Method::POST, &clone(&id), Some(json!({ "name": name })))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "name {name:?}");
    }
    let (status, _, _) = app
        .send(
            Method::POST,
            &clone("not-a-uuid"),
            Some(json!({ "name": "ok" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = app.send(Method::POST, &clone(&id), Some(json!({}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "missing name");
}

// ---------- jobs ----------

#[tokio::test]
async fn unknown_job_is_404() {
    let app = TestApp::new().await;
    let (status, body) = app.status_of(Method::GET, "/api/jobs/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["content"], "Job not found");
}

// ---------- vm-options ----------

#[tokio::test]
async fn vm_options_lists_pools_volumes_networks_and_limits() {
    let app = TestApp::new().await;
    let disk_name = format!("{}.img", unique("optdisk"));
    let iso_name = format!("{}.iso", unique("optiso"));
    let disk = create_volume(&disk_name);
    create_volume(&iso_name);

    let (status, options) = app
        .status_of(
            Method::GET,
            &format!("/api/hypervisors/{}/vm-options", app.hypervisor_id),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    let pool = options["storage_pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == POOL)
        .expect("the test pool is listed");
    assert_eq!(pool["truncated"], false);
    let volumes = pool["volumes"].as_array().unwrap();
    let find = |name: &str| {
        volumes
            .iter()
            .find(|v| v["name"] == name)
            .expect("volume listed")
    };
    assert_eq!(find(&disk_name)["kind"], "disk");
    assert_eq!(find(&disk_name)["format"], "raw");
    assert_eq!(find(&disk_name)["used_by"], json!([]));
    assert_eq!(find(&iso_name)["kind"], "iso");

    assert!(
        options["networks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "default"),
        "the default network is listed"
    );
    assert_eq!(options["firmware"], json!(["bios", "uefi"]));
    assert_eq!(options["limits"]["memory_mb"]["min"], 64);
    assert_eq!(options["limits"]["disk_gb"]["max"], 2048);
    assert_eq!(options["defaults"]["network"], "default");
    assert!(!disk.is_empty());
}

#[tokio::test]
async fn vm_options_tells_which_vms_use_a_disk() {
    let app = TestApp::new().await;
    let disk_name = format!("{}.img", unique("taken"));
    let disk = create_volume(&disk_name);
    let vm_name = unique("user");
    define_vm(&vm_name, &[&disk]);

    let (_, options) = app
        .status_of(
            Method::GET,
            &format!("/api/hypervisors/{}/vm-options", app.hypervisor_id),
        )
        .await;

    let volume = options["storage_pools"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["volumes"].as_array().unwrap())
        .find(|v| v["name"] == disk_name)
        .expect("volume listed");
    assert_eq!(volume["used_by"], json!([vm_name]));
}

#[tokio::test]
async fn vm_options_of_an_unknown_hypervisor_is_404() {
    let app = TestApp::new().await;
    let (status, _) = app
        .status_of(Method::GET, "/api/hypervisors/unknown/vm-options")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------- create ----------

impl TestApp {
    pub(crate) async fn create(&self, body: Value) -> (StatusCode, Value) {
        let (status, _, json) = self.send(Method::POST, &self.vms(), Some(body)).await;
        (status, json)
    }
}

fn domain_xml(name: &str) -> String {
    Domain::lookup_by_name(&connect(), name)
        .unwrap()
        .get_xml_desc(0)
        .unwrap()
}

#[tokio::test]
async fn create_with_a_new_disk_builds_a_stopped_vm() {
    let app = TestApp::new().await;
    let name = unique("new");

    let (status, vm) = app
        .create(json!({
            "name": name, "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "size_gb": 2 }]
        }))
        .await;

    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["name"], name);
    assert_eq!(vm["status"], "stopped");
    assert_eq!(vm["memory_mb"], 128);
    assert_eq!(vm["vcpus"], 1);
    let disk = format!("/{POOL}/{name}.qcow2");
    assert!(volume_exists(&disk), "the new disk was created");
    assert!(domain_xml(&name).contains(&disk), "and attached");
    assert!(
        domain_xml(&name).contains("type='vnc'"),
        "with a VNC display"
    );
    assert_eq!(
        app.status_of(Method::GET, &app.vm(vm["id"].as_str().unwrap()))
            .await
            .0,
        StatusCode::OK
    );

    // Deleting it with its disks leaves nothing behind.
    let url = format!("{}?delete_disks=true", app.vm(vm["id"].as_str().unwrap()));
    assert_eq!(
        app.status_of(Method::DELETE, &url).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(!volume_exists(&disk));
}

#[tokio::test]
async fn create_attaches_an_existing_disk_and_keeps_it_on_failure() {
    let app = TestApp::new().await;
    let disk_name = format!("{}.img", unique("existing"));
    let disk = create_volume(&disk_name);

    let (status, vm) = app
        .create(json!({
            "name": unique("exist"), "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "volume": disk_name }]
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert!(domain_xml(vm["name"].as_str().unwrap()).contains(&disk));

    // The disk is now taken: a second VM cannot use it, and the first one is untouched.
    let (status, body) = app
        .create(json!({
            "name": unique("second"), "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "volume": disk_name }]
        }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(volume_exists(&disk));
}

#[tokio::test]
async fn create_attaches_an_iso_and_can_start_the_vm() {
    let app = TestApp::new().await;
    let iso_name = format!("{}.iso", unique("install"));
    let iso = create_volume(&iso_name);
    let name = unique("withiso");

    let (status, vm) = app
        .create(json!({
            "name": name, "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "size_gb": 1 }],
            "iso": { "pool": POOL, "volume": iso_name },
            "start": true
        }))
        .await;

    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["status"], "running");
    let xml = domain_xml(&name);
    assert!(xml.contains(&iso));
    assert!(xml.contains("device='cdrom'"));
}

#[tokio::test]
async fn create_with_only_an_iso_is_allowed() {
    let app = TestApp::new().await;
    let iso_name = format!("{}.iso", unique("live"));
    create_volume(&iso_name);

    let (status, vm) = app
        .create(json!({
            "name": unique("live"), "memory_mb": 128, "vcpus": 1,
            "iso": { "pool": POOL, "volume": iso_name }
        }))
        .await;

    assert_eq!(status, StatusCode::CREATED, "{vm}");
}

#[tokio::test]
async fn create_refuses_a_name_already_taken() {
    let app = TestApp::new().await;
    let taken = unique("taken");
    define_vm(&taken, &[]);

    let (status, _) = app
        .create(json!({
            "name": taken, "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "size_gb": 1 }]
        }))
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        !volume_exists(&format!("/{POOL}/{taken}.qcow2")),
        "no disk created"
    );
}

#[tokio::test]
async fn create_refuses_a_volume_name_already_taken() {
    let app = TestApp::new().await;
    let name = unique("clash");
    create_volume(&format!("{name}.qcow2"));

    let (status, body) = app
        .create(json!({
            "name": name, "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "size_gb": 1 }]
        }))
        .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn create_reports_unknown_pool_volume_and_network_as_404() {
    let app = TestApp::new().await;
    let cases = [
        json!({ "disks": [{ "pool": "no-such-pool", "size_gb": 1 }] }),
        json!({ "disks": [{ "pool": POOL, "volume": "no-such-volume.img" }] }),
        json!({ "disks": [{ "pool": POOL, "size_gb": 1 }], "network": "no-such-network" }),
        json!({ "iso": { "pool": POOL, "volume": "no-such.iso" } }),
    ];
    for case in cases {
        let mut body = json!({ "name": unique("missing"), "memory_mb": 128, "vcpus": 1 });
        body.as_object_mut()
            .unwrap()
            .extend(case.as_object().unwrap().clone());
        let (status, response) = app.create(body.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body} -> {response}");
    }
}

#[tokio::test]
async fn create_refuses_volumes_of_the_wrong_kind() {
    let app = TestApp::new().await;
    let iso_name = format!("{}.iso", unique("notadisk"));
    let disk_name = format!("{}.img", unique("notaniso"));
    create_volume(&iso_name);
    create_volume(&disk_name);

    let (status, _) = app
        .create(json!({
            "name": unique("a"), "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "volume": iso_name }]
        }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an ISO is not a disk");

    let (status, _) = app
        .create(json!({
            "name": unique("b"), "memory_mb": 128, "vcpus": 1,
            "iso": { "pool": POOL, "volume": disk_name }
        }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a disk is not an ISO");
}

#[tokio::test]
async fn a_failed_creation_leaves_no_disk_behind() {
    let app = TestApp::new().await;
    let name = unique("atomic");

    // The second disk is invalid: nothing must be created for the first one either.
    let (status, _) = app
        .create(json!({
            "name": name, "memory_mb": 128, "vcpus": 1,
            "disks": [
                { "pool": POOL, "size_gb": 1 },
                { "pool": "no-such-pool", "size_gb": 1 }
            ]
        }))
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!volume_exists(&format!("/{POOL}/{name}.qcow2")));
    let (_, vms) = app.status_of(Method::GET, &app.vms()).await;
    assert!(vms.as_array().unwrap().iter().all(|vm| vm["name"] != name));
}

#[tokio::test]
async fn create_validates_the_request_before_touching_libvirt() {
    let app = TestApp::new().await;
    let invalid = [
        json!({ "name": "../evil", "memory_mb": 128, "vcpus": 1, "disks": [{ "pool": POOL, "size_gb": 1 }] }),
        json!({ "name": "ok", "memory_mb": 1, "vcpus": 1, "disks": [{ "pool": POOL, "size_gb": 1 }] }),
        json!({ "name": "ok", "memory_mb": 128, "vcpus": 0, "disks": [{ "pool": POOL, "size_gb": 1 }] }),
        json!({ "name": "ok", "memory_mb": 128, "vcpus": 1, "disks": [{ "pool": POOL, "size_gb": 99999 }] }),
        json!({ "name": "ok", "memory_mb": 128, "vcpus": 1, "disks": [] }),
        json!({ "name": "ok", "memory_mb": 128, "vcpus": 1, "disks": [{ "pool": POOL }] }),
        json!({ "name": "ok", "memory_mb": 128, "vcpus": 1, "disks": [{ "pool": POOL, "size_gb": 1 }], "firmware": "coreboot" }),
    ];
    for body in invalid {
        let (status, _) = app.create(body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    // A body that is not the expected JSON is rejected by the framework.
    let (status, _) = app.create(json!({ "name": "ok" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn create_on_an_unknown_hypervisor_is_404() {
    let app = TestApp::new().await;
    let body = json!({ "name": "x", "memory_mb": 128, "vcpus": 1, "disks": [{ "pool": POOL, "size_gb": 1 }] });
    let (status, _, _) = app
        .send(Method::POST, "/api/hypervisors/unknown/vms", Some(body))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn what_vm_options_offers_can_be_used_to_create_a_vm() {
    let app = TestApp::new().await;
    let (_, options) = app
        .status_of(
            Method::GET,
            &format!("/api/hypervisors/{}/vm-options", app.hypervisor_id),
        )
        .await;
    let network = options["networks"][0]["name"].as_str().unwrap().to_string();
    let pool = options["storage_pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == POOL)
        .unwrap();

    let (status, vm) = app
        .create(json!({
            "name": unique("roundtrip"),
            "memory_mb": options["defaults"]["memory_mb"],
            "vcpus": options["defaults"]["vcpus"],
            "disks": [{ "pool": pool["name"], "size_gb": options["defaults"]["disk_gb"] }],
            "network": network,
            "firmware": options["defaults"]["firmware"]
        }))
        .await;

    assert_eq!(status, StatusCode::CREATED, "{vm}");
}
