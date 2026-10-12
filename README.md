# KVM Multi-Hypervisor API

API Rust avec Axum pour enregistrer plusieurs hyperviseurs KVM et gerer leurs ressources.

## Fonctionnalites

- CRUD des hyperviseurs dans SQLite.
- CRUD des VM prevu via libvirt, sans stockage SQLite.
- Gestion des disques et reseaux prevue via libvirt, sans stockage SQLite.
- Documentation OpenAPI et interface Swagger.
- Verification de la connexion libvirt lors de l'ajout ou de la modification d'un hyperviseur.

Les operations libvirt reelles pour gerer les VM, disques, reseaux et VNC sont encore a brancher dans les handlers correspondants.

## Prerequis

- Linux avec KVM/libvirt.
- Rust et Cargo.
- Packages libvirt/QEMU.

Sur Debian ou Ubuntu :

```bash
sudo apt update
sudo apt install build-essential pkg-config libvirt-dev libvirt-daemon-system libvirt-clients qemu-kvm
```

Verifier libvirt :

```bash
virsh -c qemu:///system list --all
```

Ajouter l'utilisateur au groupe libvirt si necessaire, puis ouvrir une nouvelle session :

```bash
sudo usermod -aG libvirt "$USER"
```

## Installation

Depuis la racine du projet :

```bash
cargo check
```

Cargo telecharge les dependances, dont `virt`, `sqlx`, `utoipa` et Swagger UI.

## Demarrage

```bash
cargo run
```

L'API ecoute sur :

```text
http://127.0.0.1:3000
```

Arreter le serveur avec `Ctrl+C`.

## SQLite

La base est creee automatiquement au demarrage :

```text
api.db
```

Elle contient uniquement la table :

- `hypervisors`

Les VM, disques et reseaux ne sont pas ecrits dans SQLite. Leur source de verite sera libvirt lorsque les handlers seront implementes.

Ne pas supprimer `api.db` si les donnees doivent etre conservees.

## Documentation interactive

Swagger UI :

```text
http://127.0.0.1:3000/swagger-ui/
```

Document OpenAPI JSON :

```text
http://127.0.0.1:3000/api-docs/openapi.json
```

La liste complete des endpoints se trouve dans [docs/API.md](docs/API.md).

## Test rapide

Tester l'API :

```bash
curl http://127.0.0.1:3000/ping
```

Ajouter un hyperviseur local :

```bash
curl -X POST http://127.0.0.1:3000/api/hypervisors \
  -H 'Content-Type: application/json' \
  -d '{"nom":"kvm-local","uri":"qemu:///system"}'
```

Lister les hyperviseurs :

```bash
curl http://127.0.0.1:3000/api/hypervisors
```

L'URI libvirt doit etre joignable. Une URI invalide retourne `502 Bad Gateway`.

## Organisation du code

```text
src/
├── main.rs            Demarrage Axum, SQLite et Swagger
├── state.rs           Pool SQLite et creation des tables
├── models.rs          Modeles JSON et SQLx
├── routes.rs          Declaration des routes
├── hypervisors.rs     CRUD hyperviseurs et controle libvirt
├── vms.rs             Routes VM, sans stockage local
├── infrastructure.rs  Routes disques, reseaux et VNC, sans stockage local
├── common.rs          Reponses communes
└── docs.rs            Specification OpenAPI
```

## Developpement a deux

Pour limiter les conflits :

- developpeur 1 : `hypervisors.rs`, `state.rs`, migrations SQLite ;
- developpeur 2 : `vms.rs`, `infrastructure.rs`, modeles OpenAPI ;
- integration : `routes.rs`, `docs.rs`, `main.rs`.

Avant chaque commit :

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Integration continue GitHub

Le workflow [.github/workflows/ci.yml](.github/workflows/ci.yml) s'exécute automatiquement sur chaque `push` et `pull_request`.

Il vérifie :

- le formatage avec `cargo fmt` ;
- la compilation avec `cargo check` ;
- les tests avec `cargo test` ;
- les erreurs Clippy avec `cargo clippy`.

Une Pull Request ne doit pas être fusionnée si le workflow CI échoue.
