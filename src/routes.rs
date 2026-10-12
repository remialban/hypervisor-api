use axum::{
    routing::{get, post},
    Router,
};

use crate::{
    common::ping,
    hypervisors,
    infrastructure,
    state::SharedState,
    vms,
};

pub fn create_routes(state: SharedState) -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route(
            "/api/hypervisors",
            get(hypervisors::list).post(hypervisors::create),
        )
        .route(
            "/api/hypervisors/{id}",
            get(hypervisors::get)
                .patch(hypervisors::update)
                .delete(hypervisors::delete),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms",
            get(vms::list).post(vms::create),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}",
            get(vms::get).delete(vms::delete),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/start",
            post(vms::start),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop",
            post(vms::stop),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone",
            post(vms::clone),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/disks",
            get(infrastructure::list_disks).post(infrastructure::create_disk),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/disks/{disk_id}",
            get(infrastructure::get_disk)
                .patch(infrastructure::update_disk)
                .delete(infrastructure::delete_disk),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/networks",
            get(infrastructure::list_networks).post(infrastructure::create_network),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc",
            get(infrastructure::vnc),
        )
        .with_state(state)
}
