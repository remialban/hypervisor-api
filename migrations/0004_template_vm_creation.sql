-- Whether VMs may be created from a template. Existing templates keep allowing it.
ALTER TABLE templates ADD COLUMN allow_vm_creation INTEGER NOT NULL DEFAULT 1
    CHECK (allow_vm_creation IN (0, 1));
