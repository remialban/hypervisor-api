-- VM templates: a disk image built by Packer on a hypervisor, then copied for each new VM.
-- The row is also the build job: status goes pending -> in_progress -> created | failed.
CREATE TABLE templates (
    id TEXT PRIMARY KEY NOT NULL,
    hypervisor_id TEXT NOT NULL REFERENCES hypervisors (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    packer_config TEXT NOT NULL,
    -- File name of the disk image Packer writes (looked up in its output).
    disk_name TEXT NOT NULL,
    -- libvirt pool and volume holding the built disk.
    pool TEXT NOT NULL,
    volume TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'in_progress', 'created', 'failed')),
    -- Virtual size of the built disk, known once created: counted in the quota of each VM.
    disk_size_gb INTEGER,
    -- End of the Packer output when the build failed.
    error TEXT,
    created_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (hypervisor_id, name)
);
CREATE INDEX templates_by_hypervisor ON templates (hypervisor_id);
