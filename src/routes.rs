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

pub fn creer_routes(state: SharedState) -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route(
            "/api/hypervisors",
            get(hypervisors::lister).post(hypervisors::ajouter),
        )
        .route(
            "/api/hypervisors/{id}",
            get(hypervisors::obtenir)
                .patch(hypervisors::modifier)
                .delete(hypervisors::supprimer),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms",
            get(vms::lister).post(vms::creer),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}",
            get(vms::obtenir).delete(vms::supprimer),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/start",
            post(vms::demarrer),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop",
            post(vms::eteindre),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone",
            post(vms::cloner),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/disks",
            get(infrastructure::disques).post(infrastructure::creer_disque),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/disks/{disk_id}",
            get(infrastructure::obtenir_disque)
                .patch(infrastructure::modifier_disque)
                .delete(infrastructure::supprimer_disque),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/networks",
            get(infrastructure::reseaux),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc",
            get(infrastructure::vnc),
        )
        .with_state(state)
}
