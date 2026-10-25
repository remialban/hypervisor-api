-- Templates get versions: each build makes a new version with its own disk, state and log.
-- The template keeps the (editable) definition; its build fields move to template_versions.
CREATE TABLE template_versions (
    template_id TEXT NOT NULL REFERENCES templates (id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    -- Snapshot of the definition this version is built from.
    packer_config TEXT NOT NULL,
    disk_name TEXT NOT NULL,
    pool TEXT NOT NULL,
    volume TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'in_progress', 'stopping', 'created', 'failed', 'cancelled')),
    disk_size_gb INTEGER,
    error TEXT,
    -- Output of the build (Packer included), capped.
    log TEXT NOT NULL DEFAULT '',
    created_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    PRIMARY KEY (template_id, version)
);
-- At most one build at a time per template.
CREATE UNIQUE INDEX template_versions_one_build ON template_versions (template_id)
    WHERE status IN ('pending', 'in_progress', 'stopping');

-- Existing templates become their version 1. No build survives the restart applying this.
INSERT INTO template_versions (template_id, version, packer_config, disk_name, pool, volume, status, disk_size_gb, error, created_by, created_at, finished_at)
SELECT id, 1, packer_config, disk_name, pool, volume,
       CASE WHEN status IN ('pending', 'in_progress') THEN 'failed' ELSE status END,
       disk_size_gb,
       CASE WHEN status IN ('pending', 'in_progress') THEN 'The build was interrupted by a restart of the API' ELSE error END,
       created_by, created_at, updated_at
FROM templates;

ALTER TABLE templates DROP COLUMN status;
ALTER TABLE templates DROP COLUMN volume;
ALTER TABLE templates DROP COLUMN disk_size_gb;
ALTER TABLE templates DROP COLUMN error;

-- Who may use a template: everyone granted its hypervisor, or only the users listed below.
ALTER TABLE templates ADD COLUMN visibility TEXT NOT NULL DEFAULT 'hypervisor'
    CHECK (visibility IN ('hypervisor', 'restricted'));

CREATE TABLE template_access (
    template_id TEXT NOT NULL REFERENCES templates (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (template_id, user_id)
);
