# KVM Multi-Hypervisor API

API Rust avec Axum pour enregistrer plusieurs hyperviseurs KVM et gerer leurs ressources.

## Fonctionnalites

- CRUD des hyperviseurs dans SQLite.
- Lecture des VM via libvirt, sans stockage SQLite.
- Gestion des disques et reseaux prevue via libvirt, sans stockage SQLite.
- Documentation OpenAPI et interface Swagger.
- Verification de la connexion libvirt lors de l'ajout ou de la modification d'un hyperviseur.

Les VM et la configuration VNC sont lues directement depuis libvirt. Les
operations de creation, de modification et de suppression des VM, disques et
reseaux restent a implementer dans les handlers correspondants.

## Prerequis

- Linux avec KVM/libvirt.
- Rust et Cargo.
- Packages libvirt/QEMU.

Sur Debian ou Ubuntu :

```bash
sudo apt update
sudo apt install build-essential pkg-config libvirt-dev libvirt-daemon-system libvirt-clients qemu-kvm
```

Pour utiliser noVNC et websockify :

```bash
sudo apt install novnc websockify
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

Cargo telecharge les dependances, dont `virt`, `sqlx`, `roxmltree`, `url`,
`utoipa` et Swagger UI.

## Demarrage

### Demarrer l'API avec l'hote noVNC

La variable `NOVNC_HOST` indique l'adresse utilisée par l'API pour construire
l'URL noVNC retournée par l'endpoint VNC. Remplacez `192.168.122.100` par
l'adresse accessible depuis le navigateur client :

```bash
NOVNC_HOST=192.168.122.100 cargo run
```

Par défaut, si `NOVNC_HOST` n'est pas définie, l'API utilise `localhost`.

L'API ecoute sur :

```text
http://127.0.0.1:3000
```

### Demarrer websockify

Dans un autre terminal, lancez `websockify` avec le fichier de tokens généré
par l'endpoint VNC :

```bash
websockify \
  --token-plugin TokenFile \
  --token-source /home/user/Documents/api/vm.cfg \
  6080 \
  --web /usr/share/novnc
```

noVNC sera alors accessible sur :

```text
http://192.168.122.100:6080/vnc.html
```

Le fichier `vm.cfg` contient une ligne par VM au format attendu par
`websockify` :

```text
UUID_DE_LA_VM: ADRESSE_HYPERVISEUR:PORT_VNC
```

Le fichier est mis à jour lorsqu'une requête est envoyée à :

```text
GET /api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc
```

L'endpoint renvoie ensuite une propriété `novnc_url` contenant le token de la
VM. Ouvrez cette URL dans le navigateur pour établir la connexion VNC.

L'ordre recommandé est :

1. démarrer l'API avec `NOVNC_HOST` ;
2. démarrer `websockify` dans un autre terminal ;
3. appeler l'endpoint VNC pour générer ou mettre à jour la ligne dans
   `vm.cfg` ;
4. ouvrir la valeur `novnc_url` retournée par l'API.

Exemple d'appel de l'endpoint VNC :

```bash
curl http://127.0.0.1:3000/api/hypervisors/<HYPERVISOR_ID>/vms/<VM_NAME>/vnc
```

Arreter l'API ou `websockify` avec `Ctrl+C`.

### Lancer l'API et noVNC avec Docker Compose

Le Compose lance deux conteneurs distincts :

- `api` : l'API Rust sur le port `3000` ;
- `novnc` : noVNC et websockify sur le port `6080`.

Les deux conteneurs utilisent le fichier `./vm.cfg`. L'API écrit les cibles
VNC dans ce fichier, que websockify lit également. La base SQLite reste
persistée dans `./data/api.db`.

Créez un fichier `.env` à la racine du projet :

```dotenv
SSH_PRIVATE_KEY_HOST_PATH=/home/user/.ssh/id_ed25519
NOVNC_URL=http://localhost:6080/vnc.html
```

Puis lancez l'application :

```bash
mkdir -p data
touch vm.cfg
docker compose up --build
```

L'API est disponible sur `http://localhost:3000`, Swagger sur
`http://localhost:3000/swagger-ui/` et noVNC sur
`http://localhost:6080/vnc.html`.

`NOVNC_URL` est l'URL de base utilisée par l'API pour construire la propriété
`novnc_url` retournée par l'endpoint VNC. L'API ajoute automatiquement le
token et `autoconnect=true`.

Si Docker est lancé sur une machine accessible à l'adresse
`192.168.122.100`, utilisez par exemple :

```dotenv
NOVNC_URL=http://192.168.122.100:6080/vnc.html
```

Lancez ensuite :

```bash
docker compose up --build --remove-orphans
```

Le socket `/var/run/libvirt/libvirt-sock` est monté pour les hyperviseurs
locaux. La clé privée SSH est montée en lecture seule dans le backend à
`/run/secrets/ssh_private_key` pour les futures connexions libvirt via SSH.

Pour arrêter le conteneur :

```bash
docker compose down
```

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
  -d '{"name":"kvm-local","uri":"qemu:///system"}'
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
├── infrastructure.rs  Routes disques et reseaux, sans stockage local
├── vnc.rs             Routes VNC et proxy WebSocket
├── common.rs          Reponses communes
└── docs.rs            Specification OpenAPI
```

## Developpement a deux

Pour limiter les conflits :

- developpeur 1 : `hypervisors.rs`, `state.rs`, migrations SQLite ;
- developpeur 2 : `vms.rs`, modeles VM et operations libvirt VM ;
- developpeur 3 : `infrastructure.rs`, `vnc.rs`, disques, reseaux et proxy VNC ;
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
