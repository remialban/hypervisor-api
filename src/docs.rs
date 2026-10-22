use utoipa::OpenApi;

use crate::{
    common, hypervisors, infrastructure, jobs,
    models::{
        CloneVm, CreateHypervisor, CreateVm, Disk, DiskRequest, DiskSpec, DiskUpdateRequest,
        Hypervisor, Message, Network, NetworkRequest, StoragePool, StoragePoolRequest,
        UpdateHypervisor, VirtualMachine, VncResponse, VolumeRef,
    },
    vm_options, vms, vnc,
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "KVM Multi-Hypervisor API",
        version = "0.1.0",
        description = "API for managing KVM hypervisors, virtual machines, disks, networks and VNC"
    ),
    paths(
        common::ping,
        hypervisors::list,
        hypervisors::create,
        hypervisors::get,
        hypervisors::update,
        hypervisors::delete,
        vms::list,
        vms::get,
        vms::create,
        vms::start,
        vms::stop,
        vms::clone,
        vms::delete,
        infrastructure::list_disks,
        infrastructure::create_disk,
        infrastructure::list_pools,
        infrastructure::create_pools,
        infrastructure::get_disk,
        infrastructure::update_disk,
        infrastructure::delete_disk,
        infrastructure::list_networks,
        infrastructure::create_network,
        vnc::connect,
        jobs::get,
        vm_options::get
    ),
    components(schemas(
        Message,
        Hypervisor,
        CreateHypervisor,
        UpdateHypervisor,
        CreateVm,
        DiskSpec,
        VolumeRef,
        VirtualMachine,
        Disk,
        DiskRequest,
        DiskUpdateRequest,
        StoragePool,
        StoragePoolRequest,
        Network,
        NetworkRequest,
        VncResponse,
        CloneVm,
        jobs::Job,
        jobs::JobStatus,
        vm_options::VmOptions,
        vm_options::PoolOption,
        vm_options::VolumeOption,
        vm_options::VolumeKind,
        vm_options::NetworkOption,
        vm_options::Range,
        vm_options::Limits,
        vm_options::Defaults
    )),
    tags(
        (name = "Hypervisors", description = "Hypervisor management"),
        (name = "VMs", description = "Virtual machine management"),
        (name = "Jobs", description = "Background operations"),
        (name = "Infrastructure", description = "Disks, networks and VNC")
    )
)]
pub struct ApiDoc;
