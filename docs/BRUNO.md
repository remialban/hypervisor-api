# Using Bruno with this API

[Bruno](https://www.usebruno.com/) is an open-source API client. Collections are plain text files (`.bru`) stored in the repository, so the collection in [`docs/bruno`](bruno/) is versioned and reviewed like code.
For the collection's content, variables and how to keep it in sync with Swagger, see [`bruno/README.md`](bruno/README.md).

## 1. Prerequisites

- Bruno desktop app: download from <https://www.usebruno.com/downloads>, or on macOS `brew install --cask bruno`.
- The API running locally:
  ```bash
  cargo run          # listens on http://127.0.0.1:3000
  ```
  Check it with `curl http://127.0.0.1:3000/ping` → `{"status":"ok","content":"pong"}`.
- Optional, for the command line: Node.js, then `npm install -g @usebruno/cli`.

## 2. Open the collection

1. In Bruno: **Collections → Open Collection**.
2. Select the `docs/bruno` folder of this repository (the one containing `bruno.json`).
3. The collection **KVM Multi-Hypervisor API** appears in the sidebar, with one folder per area (Health, Hypervisors, VMs, Disks, Networks, VNC, Error cases).

Nothing else to import: no account, no sync, no file outside the repository.

## 3. Select an environment

Top-right dropdown → choose **local**.

Environments hold the variables used in every URL and body (`{{baseUrl}}`, `{{apiPrefix}}`, `{{libvirtUri}}`, …). To change the target server, edit the environment instead of editing requests:

- **Environments → Configure** (or the gear icon next to the dropdown).
- Change `baseUrl` (e.g. `http://192.168.1.50:3000`) and save.
- To add a new target, create a new environment (or copy `environments/local.bru` and rename it).

Hover any `{{variable}}` in a request: green means it resolves, red means it is empty or unknown.

## 4. Send your first requests

Recommended order for a first run:

1. **00-Health → Ping**: press **Send** (or `Ctrl/Cmd + Enter`). Expect `200` and `pong`.
2. **01-Hypervisors → Create hypervisor**: registers the `libvirtUri` hypervisor. Expect `201`.
   A post-response script saves the returned `id` into the runtime variable `hypervisorId`, which every other request reuses.
3. **List / Get / Update hypervisor**: should return `200`.
4. **VMs, Disks, Networks, VNC**: currently answer `501 not_implemented`. This is expected until those handlers are written.
5. **06-Error-cases**: shows the 404, 502 and 422 responses a frontend must handle.
6. **Delete hypervisor**: run it last, since it removes the hypervisor the other requests depend on.

Each request has a **Docs** tab describing its body, success response and error codes, and an **Assert** tab showing the expected status. After sending, the **Tests** result in the response pane shows pass/fail.

## 5. Useful features

| Need | Where |
|---|---|
| See the raw request/response, headers, timeline | Response pane tabs |
| Edit a JSON body | **Body** tab → JSON (variables like `{{libvirtUri}}` work inside it) |
| Inspect variables currently set (including `hypervisorId`) | Eye icon (top right) |
| Run a whole folder in order | Right-click the folder → **Run** |
| Run everything | Right-click the collection → **Run** |
| Generate client code (curl, JS, …) | Right-click a request → **Generate Code** |
| Search requests | `Ctrl/Cmd + K` |

Runtime variables (set by scripts, such as `hypervisorId`) take priority over the environment's value and are cleared when Bruno closes. Restarting the API does not clear them, but the hypervisor might no longer exist if you deleted `api.db`: just run **Create hypervisor** again.

## 6. Command line (CI or quick checks)

From the collection folder:

```bash
cd docs/bruno
bru run --env local                      # whole collection
bru run 01-Hypervisors --env local       # one folder
bru run 06-Error-cases --env local       # error cases only
bru run --env local --env-var baseUrl=http://192.168.1.50:3000   # override a variable
```

Requests run in the order of their `seq` numbers (folder numbers first). A full run ends with *Delete hypervisor* and expects every assertion to pass, so with the current code `VMs`, `Disks`, `Networks` and `VNC` pass only because they assert `501`.

To check that the collection still matches the API's Swagger spec:

```bash
docs/bruno/check-sync.sh      # API must be running; needs curl and jq
```

## 7. Editing the collection

- Requests are text files: edit them in Bruno or in your editor, then commit them like any other file.
- When an endpoint becomes implemented, open its request, update the **Assert** (status code) and the **Docs** tab, and add any new body fields.
- New endpoint: duplicate a similar request (right-click → **Clone**), then change its name, method and URL. Keep `{{baseUrl}}{{apiPrefix}}` at the start of the URL and use `{{hypervisorId}}`-style variables for path parameters.
- Do not put secrets in environment files (they are committed). Use a local `docs/bruno/.env` (git-ignored, template in `.env.example`) and reference it with `{{process.env.NAME}}`.

## 8. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `ECONNREFUSED` | The API is not running or `baseUrl` is wrong. Start it with `cargo run`. |
| `{{hypervisorId}}` stays red, requests return 404 | Run **Create hypervisor** first, or the hypervisor was deleted. |
| `502 Could not connect to libvirt with this URI` | `libvirtUri` is unreachable. For local work keep `test:///default` (libvirt's built-in mock driver). Real hosts need `qemu+ssh://user@host/system` and working SSH access. |
| `422 Failed to deserialize the JSON body…` | A required field is missing or has the wrong type (e.g. `vcpus` must be a number). The message is plain text from Axum. |
| `501 not_implemented` | The endpoint exists but is not wired to libvirt yet. |
| Collection does not open | Select the folder containing `bruno.json` (`docs/bruno`), not `docs`. |
