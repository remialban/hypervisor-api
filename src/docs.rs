use utoipa::OpenApi;

use crate::{
    common,
    hypervisors,
    infrastructure,
    models::{CreateHypervisor, CreateVm, DiskRequest, Hypervisor, Message, UpdateHypervisor},
    vms,
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "KVM Multi-Hypervisor API",
        version = "0.1.0",
        description = "API de gestion des hyperviseurs KVM, VM, disques, reseaux et VNC"
    ),
    paths(
        common::ping,
        hypervisors::lister,
        hypervisors::ajouter,
        hypervisors::obtenir,
        hypervisors::modifier,
        hypervisors::supprimer,
        vms::lister,
        vms::obtenir,
        vms::creer,
        vms::demarrer,
        vms::eteindre,
        vms::cloner,
        vms::supprimer,
        infrastructure::disques,
        infrastructure::creer_disque,
        infrastructure::obtenir_disque,
        infrastructure::modifier_disque,
        infrastructure::supprimer_disque,
        infrastructure::reseaux,
        infrastructure::vnc
    ),
    components(schemas(
        Message,
        Hypervisor,
        CreateHypervisor,
        UpdateHypervisor,
        CreateVm,
        DiskRequest
    )),
    tags(
        (name = "Hypervisors", description = "Gestion des hyperviseurs"),
        (name = "VMs", description = "Gestion des machines virtuelles"),
        (name = "Infrastructure", description = "Disques, reseaux et VNC")
    )
)]
pub struct ApiDoc;