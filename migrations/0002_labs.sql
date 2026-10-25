-- Labs: a set of VMs made from templates, wired to networks, deployed as instances.
-- The API refuses to remove a network or a VM still in use (by a VM, an instance): the
-- cascades below only matter when a whole lab goes.
--
-- A lab is a definition (its networks, its VMs and the networks of each VM). Deploying it
-- makes an instance: the internal networks are created on the hypervisor, and every VM is
-- created from its template and owned by whoever deployed the instance.

CREATE TABLE labs (
    id TEXT PRIMARY KEY,
    hypervisor_id TEXT NOT NULL REFERENCES hypervisors (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    -- Who may deploy it besides admins and the rights below: everyone granted its
    -- hypervisor, or only through rights.
    visibility TEXT NOT NULL DEFAULT 'restricted' CHECK (visibility IN ('hypervisor', 'restricted')),
    created_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE (hypervisor_id, name)
);

-- `external`: an existing libvirt network of the hypervisor (NAT or bridge to the outside),
-- shared by every instance. `internal`: an isolated network, created for each instance.
CREATE TABLE lab_networks (
    id TEXT PRIMARY KEY,
    lab_id TEXT NOT NULL REFERENCES labs (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('external', 'internal')),
    libvirt_network TEXT,
    created_at BIGINT NOT NULL,
    UNIQUE (lab_id, name),
    CHECK ((kind = 'external') = (libvirt_network IS NOT NULL))
);

-- `router`: a VM routing between the networks of the lab (OPNsense...). It is started
-- before the others. version NULL: the latest built version of the template.
CREATE TABLE lab_vms (
    id TEXT PRIMARY KEY,
    lab_id TEXT NOT NULL REFERENCES labs (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'vm' CHECK (role IN ('vm', 'router')),
    template_id TEXT NOT NULL REFERENCES templates (id),
    version BIGINT,
    memory_mb BIGINT NOT NULL,
    vcpus BIGINT NOT NULL,
    firmware TEXT NOT NULL DEFAULT 'bios' CHECK (firmware IN ('bios', 'uefi')),
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE (lab_id, name)
);
CREATE INDEX lab_vms_by_template ON lab_vms (template_id);

-- The NICs of a lab VM, in order: position 0 is the first NIC the guest sees (the WAN of
-- an OPNsense, for instance).
CREATE TABLE lab_vm_nics (
    lab_vm_id TEXT NOT NULL REFERENCES lab_vms (id) ON DELETE CASCADE,
    position BIGINT NOT NULL,
    network_id TEXT NOT NULL REFERENCES lab_networks (id) ON DELETE CASCADE,
    PRIMARY KEY (lab_vm_id, position)
);
CREATE INDEX lab_vm_nics_by_network ON lab_vm_nics (network_id);

-- Rights on a lab, like the rights on a template. `use`: deploy instances and act on them;
-- `manage`: also edit the lab and see every instance. max_instances: how many instances of
-- the lab each user covered may own (NULL = no limit).
CREATE TABLE lab_permissions (
    lab_id TEXT NOT NULL REFERENCES labs (id) ON DELETE CASCADE,
    user_id TEXT REFERENCES users (id) ON DELETE CASCADE,
    group_id TEXT REFERENCES user_groups (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('use', 'manage')),
    max_instances BIGINT CHECK (max_instances IS NULL OR max_instances >= 0),
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    CHECK ((user_id IS NULL) <> (group_id IS NULL))
);
CREATE UNIQUE INDEX lab_permissions_by_user ON lab_permissions (lab_id, user_id)
    WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX lab_permissions_by_group ON lab_permissions (lab_id, group_id)
    WHERE group_id IS NOT NULL;

-- A deployed lab. status: deploying -> ready | failed.
CREATE TABLE lab_instances (
    id TEXT PRIMARY KEY,
    lab_id TEXT NOT NULL REFERENCES labs (id),
    owner_id TEXT NOT NULL REFERENCES users (id),
    name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('deploying', 'ready', 'failed')),
    error TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE INDEX lab_instances_by_lab ON lab_instances (lab_id);
CREATE INDEX lab_instances_by_owner ON lab_instances (owner_id);

-- The libvirt network each lab network got in an instance (created for an internal one).
CREATE TABLE lab_instance_networks (
    instance_id TEXT NOT NULL REFERENCES lab_instances (id) ON DELETE CASCADE,
    network_id TEXT NOT NULL REFERENCES lab_networks (id) ON DELETE CASCADE,
    libvirt_network TEXT NOT NULL,
    created BOOLEAN NOT NULL,
    PRIMARY KEY (instance_id, network_id)
);

-- The VMs of an instance. The row goes with the VM's owner row (VM deleted).
CREATE TABLE lab_instance_vms (
    instance_id TEXT NOT NULL REFERENCES lab_instances (id) ON DELETE CASCADE,
    lab_vm_id TEXT NOT NULL REFERENCES lab_vms (id) ON DELETE CASCADE,
    hypervisor_id TEXT NOT NULL,
    vm_id TEXT NOT NULL,
    name TEXT NOT NULL,
    pool TEXT NOT NULL,
    volume TEXT NOT NULL,
    PRIMARY KEY (instance_id, lab_vm_id),
    UNIQUE (hypervisor_id, vm_id),
    FOREIGN KEY (hypervisor_id, vm_id) REFERENCES vm_owners (hypervisor_id, vm_id) ON DELETE CASCADE
);
