use utoipa::OpenApi;

use crate::{
    common,
    hypervisors,
    infrastructure,
    models::{
        CreateHypervisor, CreateVm, Disk, DiskRequest, Hypervisor, Message, Network,
        NetworkRequest, UpdateHypervisor, VirtualMachine,
    },
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
        infrastructure::creer_reseau,
        infrastructure::vnc
    ),
    components(schemas(
        Message,
        Hypervisor,
        CreateHypervisor,
        UpdateHypervisor,
        CreateVm,
        VirtualMachine,
        Disk,
        DiskRequest,
        Network,
        NetworkRequest
    )),
    tags(
        (name = "Hypervisors", description = "Gestion des hyperviseurs"),
        (name = "VMs", description = "Gestion des machines virtuelles"),
        (name = "Infrastructure", description = "Disques, reseaux et VNC")
    )
)]
pub struct ApiDoc;