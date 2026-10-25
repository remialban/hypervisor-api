-- Rights on a template, given to a user or to a group (and so to every user in it,
-- nested groups included). `use`: see it and create VMs from it; `manage`: also edit,
-- rebuild and stop it and read its logs. max_vms: how many VMs from this template each
-- user covered may own (NULL = no limit).
CREATE TABLE template_permissions (
    template_id TEXT NOT NULL REFERENCES templates (id) ON DELETE CASCADE,
    user_id TEXT REFERENCES users (id) ON DELETE CASCADE,
    group_id TEXT REFERENCES user_groups (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('use', 'manage')),
    max_vms INTEGER CHECK (max_vms IS NULL OR max_vms >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK ((user_id IS NULL) <> (group_id IS NULL))
);
CREATE UNIQUE INDEX template_permissions_by_user ON template_permissions (template_id, user_id)
    WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX template_permissions_by_group ON template_permissions (template_id, group_id)
    WHERE group_id IS NOT NULL;

-- The users listed for a restricted template keep using it, without limit.
INSERT INTO template_permissions (template_id, user_id, role, max_vms, created_at, updated_at)
SELECT template_id, user_id, 'use', NULL, created_at, created_at FROM template_access;
DROP TABLE template_access;

-- Which template each VM was created from, to count it in the limits above. The row goes
-- with the VM's owner row (VM deleted) or with the template.
CREATE TABLE template_vms (
    hypervisor_id TEXT NOT NULL,
    vm_id TEXT NOT NULL,
    template_id TEXT NOT NULL REFERENCES templates (id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (hypervisor_id, vm_id),
    FOREIGN KEY (hypervisor_id, vm_id) REFERENCES vm_owners (hypervisor_id, vm_id) ON DELETE CASCADE
);
CREATE INDEX template_vms_by_template ON template_vms (template_id);
