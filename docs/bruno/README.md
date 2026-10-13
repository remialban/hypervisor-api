# Bruno collection – KVM Multi-Hypervisor API

Collection for frontend developers, derived **only from the Rust source** (`src/routes.rs`, `src/models.rs`, handlers).
No formal contract exists yet: this collection is the working draft that will help define it.

## Use it

1. Install [Bruno](https://www.usebruno.com/) and run the API (`cargo run`, listens on `http://127.0.0.1:3000`).
2. In Bruno: **Open Collection** → select this `docs/bruno` folder.
3. Pick the environment `local` (top right), then run **01-Hypervisors → Create hypervisor** first.
   It stores `hypervisorId` as a runtime variable used by every other request.
4. Run **Delete hypervisor** last, since it removes the hypervisor the other folders rely on.

## Variables (`environments/*.bru`)

| Variable | Purpose | Default (`local`) |
|---|---|---|
| `baseUrl` | API host | `http://127.0.0.1:3000` |
| `apiPrefix` | Prefix of all resource routes (`/ping` is outside it) | `/api` |
| `libvirtUri` | libvirt URI sent when creating a hypervisor | `test:///default` (libvirt mock driver, no KVM needed) |
| `hypervisorId` | Set automatically by *Create hypervisor* | empty |
| `vmId`, `diskId`, `networkId` | Fill in by hand (or set via scripts) once those endpoints are implemented | empty |
| `missingId` | An id that never exists, used by the *Error cases* folder | all-zero UUID |

To target another API instance, duplicate an environment file and change `baseUrl`. `remote-kvm` is a template for a real host (`qemu+ssh://...`).
No secrets are needed today. If auth is added, use `{{process.env.NAME}}` with a local `.env` (see `.env.example`; `.env` is git-ignored).

## Endpoint status (from the code)

| Folder | State |
|---|---|
| Health, Hypervisors | Implemented (SQLite + libvirt connection test) |
| VMs, Disks, Networks, VNC (own Swagger tag and folder) | Return `501 {"status":"not_implemented","content":...}`. Assertions expect 501 for now. |
| Error cases | 502 unreachable URI, 404 unknown id, 422 invalid body (framework message, plain text, not a `Message`) |

When an endpoint gets implemented, change its `assert` status and body in the `.bru` file.

## Keeping it in sync with Swagger

The API publishes its spec at `http://127.0.0.1:3000/api-docs/openapi.json` (UI: `/swagger-ui`).

- **Detect drift** (API running): `docs/bruno/check-sync.sh` lists endpoints present in Swagger but missing here, and the reverse. Exit code 1 on drift, so it can run in CI. It also accepts a spec file or URL as an argument. Requires `curl` and `jq`.
- **Add a new endpoint**: copy a similar `.bru` file into the right folder, change `meta.name`, the method block and the URL (keep `{{baseUrl}}{{apiPrefix}}`; use `{{hypervisorId}}`-style variables for path params), then re-run `check-sync.sh`.
- **Bulk update**: in Bruno, *Import Collection → OpenAPI* with the URL above into a scratch folder, then copy the new requests over, replacing hard-coded hosts with the variables above.

## Layout

```
docs/bruno/
├── bruno.json / collection.bru     collection config and shared headers
├── environments/                   local.bru, remote-kvm.bru
├── 00-Health … 06-Error-cases/     one folder per Swagger tag/concern, numbered for run order
├── check-sync.sh                   Swagger vs collection drift check
└── .env.example
```

Known quirk: `PATCH /disks/{disk_id}` reuses `DiskRequest`, so all three fields are required (no partial update in the code today), whereas `PATCH /hypervisors/{id}` accepts optional fields.
