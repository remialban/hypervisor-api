-- Hypervisors existed before the migrations: IF NOT EXISTS keeps existing databases working.
CREATE TABLE IF NOT EXISTS hypervisors (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    uri TEXT NOT NULL,
    status TEXT NOT NULL
);

-- Quotas: NULL means "no limit" (the default).
CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin', 'user')),
    disabled INTEGER NOT NULL DEFAULT 0,
    max_vms INTEGER,
    max_vcpus INTEGER,
    max_memory_mb INTEGER,
    max_disk_gb INTEGER,
    created_at INTEGER NOT NULL
);

-- Only the SHA-256 of a refresh token is stored: a leaked database gives no usable token.
CREATE TABLE refresh_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

-- An admin gives a user the right to create VMs on a hypervisor.
CREATE TABLE hypervisor_access (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    hypervisor_id TEXT NOT NULL REFERENCES hypervisors (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, hypervisor_id)
);

-- libvirt has no notion of owner: it lives here, keyed by the libvirt UUID.
-- vcpus / memory_mb / disk_gb remember what the VM was created with, to count quotas.
CREATE TABLE vm_owners (
    hypervisor_id TEXT NOT NULL REFERENCES hypervisors (id) ON DELETE CASCADE,
    vm_id TEXT NOT NULL,
    owner_id TEXT NOT NULL REFERENCES users (id),
    vcpus INTEGER NOT NULL DEFAULT 0,
    memory_mb INTEGER NOT NULL DEFAULT 0,
    disk_gb INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (hypervisor_id, vm_id)
);
CREATE INDEX vm_owners_by_owner ON vm_owners (owner_id);

-- A VM lent to someone: console + start/stop, nothing else. expires_at NULL = no end date.
CREATE TABLE vm_shares (
    hypervisor_id TEXT NOT NULL,
    vm_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    PRIMARY KEY (hypervisor_id, vm_id, user_id),
    FOREIGN KEY (hypervisor_id, vm_id) REFERENCES vm_owners (hypervisor_id, vm_id) ON DELETE CASCADE
);
CREATE INDEX vm_shares_by_user ON vm_shares (user_id);

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO settings (key, value) VALUES ('registration_open', 'false');

-- Short-lived tokens accepted by websockify. The token file is generated from this table.
CREATE TABLE vnc_tokens (
    token TEXT PRIMARY KEY NOT NULL,
    target TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    user_id TEXT,
    username TEXT,
    action TEXT NOT NULL,
    target TEXT,
    detail TEXT,
    success INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX audit_log_by_time ON audit_log (at);
