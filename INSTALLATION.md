# Installation

Ce guide explique comment faire tourner l'API, de deux façons :

- **[Option 1 — Développement](#option-1--développement)** : l'API lancée avec `cargo run`
  sur votre machine, pour modifier le code et le tester ;
- **[Option 2 — Installation avec Docker](#option-2--installation-avec-docker)** : l'API,
  noVNC et HAProxy (HTTPS) lancés avec Docker Compose, pour l'utiliser au quotidien ou la
  déployer sur un serveur.

Dans les deux cas, l'API pilote des hyperviseurs KVM via libvirt. La
[dernière partie](#préparer-un-hyperviseur) explique comment préparer un hyperviseur.

---

## Option 1 — Développement

### 1. Prérequis

- Linux avec KVM et libvirt.
- Rust 1.88 ou plus récent (édition 2024 et `if let … &&`) : <https://rustup.rs>.
- Les en-têtes de libvirt et `pkg-config`, pour compiler la crate `virt`.

Debian / Ubuntu :

```bash
sudo apt update
sudo apt install build-essential pkg-config libvirt-dev libvirt-daemon-system \
  libvirt-clients qemu-kvm
```

Arch Linux :

```bash
sudo pacman -S --needed base-devel pkgconf libvirt qemu-full
sudo systemctl enable --now libvirtd
```

Ajoutez votre utilisateur au groupe `libvirt`, puis ouvrez une nouvelle session :

```bash
sudo usermod -aG libvirt "$USER"
virsh -c qemu:///system list --all    # doit répondre sans demander de mot de passe
```

En option :

- **noVNC et websockify**, pour ouvrir les consoles des VM : `sudo apt install novnc websockify`
  (Arch : `sudo pacman -S websockify`, et noVNC depuis l'AUR ou <https://github.com/novnc/noVNC>) ;
- **Packer**, `qemu-img` et `virsh`, pour construire des templates : voir
  <https://developer.hashicorp.com/packer/install>.

### 2. Compiler et lancer l'API

Depuis la racine du projet :

```bash
ADMIN_PASSWORD='un-mot-de-passe' \
JWT_SECRET="$(openssl rand -base64 48)" \
cargo run
```

L'API écoute sur `http://127.0.0.1:3000`. Au premier démarrage, la base SQLite `api.db` est
créée dans le dossier courant, les migrations sont appliquées, et un administrateur `admin`
est créé avec `ADMIN_PASSWORD`. Sans `ADMIN_PASSWORD`, un mot de passe est généré et affiché
**une seule fois** dans la console.

Sans `JWT_SECRET`, une clé aléatoire est utilisée à chaque démarrage : tout le monde est
déconnecté quand l'API redémarre. C'est sans conséquence en développement.

Vérifiez :

```bash
curl http://127.0.0.1:3000/ping       # {"status":"ok","content":"pong"}
```

Swagger est sur <http://127.0.0.1:3000/swagger-ui/> : bouton **Authorize**, puis
`admin` / votre mot de passe.

### 3. Se connecter et ajouter un hyperviseur

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:3000/api/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"un-mot-de-passe"}' | jq -r .access_token)

# Hyperviseur local
curl -X POST http://127.0.0.1:3000/api/hypervisors \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"kvm-local","uri":"qemu:///system"}'
```

Pour un hyperviseur distant, utilisez `qemu+ssh://utilisateur@adresse/system` (voir
[Préparer un hyperviseur](#préparer-un-hyperviseur)). L'API refuse (`502`) une URI qu'elle
ne peut pas joindre.

### 4. Ouvrir les consoles (noVNC), en option

Dans un second terminal, lancez websockify avec le fichier de jetons que l'API écrit
(`vm.cfg` dans le dossier courant, ou `VM_CONFIG_PATH`) :

```bash
websockify --token-plugin TokenFile --token-source "$PWD/vm.cfg" \
  6080 --web /usr/share/novnc
```

Lancez alors l'API avec l'adresse de noVNC vue par votre navigateur :

```bash
NOVNC_URL=http://localhost:6080/vnc.html ADMIN_PASSWORD='…' cargo run
```

`GET /api/hypervisors/{id}/vms/{vm_id}/vnc` renvoie une `novnc_url` valable deux minutes :
ouvrez-la dans le navigateur.

### 5. Tests et qualité

Les tests n'ont besoin d'aucun hyperviseur : ils utilisent le pilote en mémoire de libvirt
(`test:///default`) et une base SQLite en mémoire.

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

Les tests d'arrêt de build de templates lancent un vrai `packer` s'il est installé ; sinon
ils sont ignorés.

La collection [Bruno](docs/BRUNO.md) (`docs/bruno/`) contient une requête pour chaque route,
avec l'environnement `local` déjà pointé sur `http://127.0.0.1:3000`.

### Variables d'environnement

| Variable | Défaut | Rôle |
|---|---|---|
| `DATABASE_URL` | `sqlite://api.db?mode=rwc` | Base SQLite |
| `JWT_SECRET` (ou `JWT_SECRET_FILE`) | aléatoire | Signe les jetons d'accès (32 caractères minimum) |
| `ADMIN_USERNAME` / `ADMIN_PASSWORD` | `admin` / généré | Premier administrateur, si la base est vide |
| `ACCESS_TOKEN_TTL_SECONDS` | `900` | Durée d'un jeton d'accès |
| `REFRESH_TOKEN_TTL_SECONDS` | `604800` | Durée d'une session (refresh token) |
| `VNC_TOKEN_TTL_SECONDS` | `120` | Durée d'un jeton noVNC |
| `VM_CONFIG_PATH` | `vm.cfg` | Fichier de jetons lu par websockify |
| `NOVNC_URL` | — | URL de `vnc.html` vue par le navigateur |
| `NOVNC_HOST` | `localhost` | Utilisé si `NOVNC_URL` est absente : `http://<NOVNC_HOST>:6080/vnc.html` |
| `VNC_LISTEN` | `0.0.0.0` | Adresse d'écoute VNC des VM créées par l'API |
| `CORS_ALLOWED_ORIGINS` | vide | Origines du front autorisées, séparées par des virgules |
| `PACKER_TIMEOUT_SECONDS` | `7200` | Durée maximale d'un build de template |

Le détail de l'authentification (rôles, quotas, branchement du front) est dans
[docs/AUTH.md](docs/AUTH.md).

---

## Option 2 — Installation avec Docker

Docker Compose lance quatre conteneurs :

| Conteneur | Rôle | Accès |
|---|---|---|
| `api-haproxy` | Entrée HTTPS de l'API (Swagger, Bruno, scripts) | `8443` (HTTPS), `8081` (redirigé vers HTTPS) |
| `novnc-haproxy` | Entrée HTTPS des consoles | `6443` (HTTPS), `6081` (redirigé vers HTTPS) |
| `api` | L'API | `127.0.0.1:3000` seulement (outils locaux) |
| `novnc` | noVNC et websockify | aucun port publié : uniquement via `novnc-haproxy` |

Les ports 80 et 443 sont laissés au frontend (`hypervisor-ui`), qui a son propre HAProxy et
joint l'API par le réseau Docker. Le détail des HAProxy est dans
[docs/HAPROXY.md](docs/HAPROXY.md).
Le fonctionnement avec le frontend (`hypervisor-ui`), avec schémas des conteneurs, ports et
réseaux, est dans [docs/DOCKER.md](docs/DOCKER.md).

### 1. Prérequis

- Docker Engine et le plugin Docker Compose v2 (`docker compose version`).
- Sur la machine Docker : une clé SSH qui ouvre une session sur vos hyperviseurs, et un
  fichier `~/.ssh/known_hosts` qui les connaît (voir
  [Préparer un hyperviseur](#préparer-un-hyperviseur)).
- Pour un hyperviseur local : libvirt installé sur la machine Docker (le socket
  `/var/run/libvirt/libvirt-sock` est monté dans l'API).

### 2. Le certificat HTTPS

Les deux HAProxy partagent un certificat et sa clé privée, en PEM, dans **deux fichiers**.
La clé doit être lisible par l'UID 99 (l'utilisateur de HAProxy dans son conteneur).

**Pour un essai ou un réseau local**, un certificat auto-signé suffit. Mettez dans
`subjectAltName` les noms et adresses par lesquels les navigateurs joindront le serveur :

```bash
mkdir -p tls
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 365 \
  -subj "/CN=kvm.local" \
  -addext "subjectAltName=DNS:localhost,DNS:kvm.local,IP:127.0.0.1,IP:192.168.1.10" \
  -keyout tls/key.pem -out tls/cert.pem
sudo chown 99:99 tls/key.pem && sudo chmod 400 tls/key.pem
```

Le dossier `tls/` est ignoré par git et par Docker : la clé ne finit ni dans un commit ni
dans une image.

**Pour un accès public**, utilisez un vrai certificat (Let's Encrypt…) : voir
[docs/HAPROXY.md](docs/HAPROXY.md#avec-un-certificat-existant-lets-encrypt-autorité-interne).

### 3. Le fichier `.env`

```bash
cp .env.example .env
```

Puis **remplacez toutes les valeurs d'exemple**. Un chemin d'exemple laissé tel quel
(`/home/user/…`, `/etc/hypervisor-api/…`) fait échouer le démarrage avec
`bind source path does not exist`.

```dotenv
# Images (laissez tel quel si vous construisez avec --build)
DOCKERHUB_USERNAME=remi
TAG=latest

# Clé SSH vers les hyperviseurs : le chemin réel, sur la machine Docker
SSH_PRIVATE_KEY_HOST_PATH=/home/<vous>/.ssh/id_ed25519

# HTTPS
TLS_CERT_PATH=./tls/cert.pem
TLS_KEY_PATH=./tls/key.pem

# Ports publiés : 80 et 443 sont laissés au frontend
API_HTTP_PORT=8081
API_HTTPS_PORT=8443
NOVNC_HTTP_PORT=6081
NOVNC_HTTPS_PORT=6443

# Adresse des consoles vue par les navigateurs : le HAProxy de noVNC (port 6443),
# avec un nom ou une IP présent dans le certificat
NOVNC_URL=https://192.168.1.10:6443/vnc.html

# Secrets : à générer, jamais les valeurs d'exemple
JWT_SECRET=<résultat de : openssl rand -base64 48>
ADMIN_USERNAME=admin
ADMIN_PASSWORD=<mot de passe du premier administrateur>

# Vide : le frontend appelle l'API par son propre nginx, sur sa propre origine
CORS_ALLOWED_ORIGINS=
```

`ADMIN_USERNAME` et `ADMIN_PASSWORD` ne servent qu'au tout premier démarrage, quand la base
est vide. Changez ensuite le mot de passe avec `PUT /api/me/password`.

### 4. Démarrer

```bash
docker compose up -d --build --remove-orphans
docker compose ps
```

Les quatre conteneurs doivent être `Up`. `--build` construit les images de l'API et de noVNC
depuis le code ; sans lui, Compose peut prendre une image plus ancienne.

### 5. Vérifier

```bash
curl --cacert tls/cert.pem https://localhost:8443/ping            # {"status":"ok","content":"pong"}
curl -I http://localhost:8081/ping                               # 301 vers https://…:8443
curl --cacert tls/cert.pem -o /dev/null -w '%{http_code}\n' https://localhost:6443/vnc.html   # 200
```

(`--cacert` n'est utile qu'avec le certificat auto-signé.)

Dans le navigateur :

- Swagger : `https://<serveur>:8443/swagger-ui/` ;
- consoles : la `novnc_url` renvoyée par `GET /api/hypervisors/{id}/vms/{vm_id}/vnc`
  (`https://<serveur>:6443/vnc.html?…`) ;
- l'application : `https://<serveur>`, une fois le frontend lancé (voir
  [docs/DOCKER.md](docs/DOCKER.md#démarrer-arrêter)).

Avec le certificat auto-signé, le navigateur affiche un avertissement **sur chaque port** :
acceptez-le sur 8443 et sur 6443 (sinon les consoles restent vides), ou importez
`tls/cert.pem` comme autorité de confiance.

Connectez-vous avec `POST /api/auth/login`, puis ajoutez vos hyperviseurs comme dans
l'[option 1](#3-se-connecter-et-ajouter-un-hyperviseur), en remplaçant
`http://127.0.0.1:3000` par `https://<serveur>:8443`.

### 6. Exploitation

```bash
docker compose logs -f              # tous les logs
docker compose logs -f api-haproxy novnc-haproxy   # une ligne par requête, avec le code
docker compose restart api-haproxy novnc-haproxy   # après un changement de certificat
docker compose down                 # arrêter (les données restent)
```

**Mettre à jour** :

```bash
git pull
docker compose up -d --build --remove-orphans
```

Les migrations de la base s'appliquent toutes seules au démarrage de l'API.

**Sauvegarder** : `./data/api.db` contient les comptes, les groupes, les droits, les
propriétaires des VM, les templates et le journal d'audit. Les VM et leurs disques sont, eux,
sur les hyperviseurs. Changer `JWT_SECRET` déconnecte tout le monde mais ne perd rien.

### 7. Problèmes courants

| Symptôme | Cause et solution |
|---|---|
| `bind source path does not exist` | Un chemin du `.env` (`TLS_CERT_PATH`, `TLS_KEY_PATH`, `SSH_PRIVATE_KEY_HOST_PATH`) n'existe pas sur la machine Docker. |
| `checking context: no permission to read …` pendant le build | Un fichier illisible est dans le dossier du projet hors de `tls/` : déplacez-le, ou ajoutez-le à `.dockerignore`. |
| HAProxy redémarre en boucle : `no SSL certificate specified` | Le chemin du certificat désigne un dossier, pas un fichier. |
| HAProxy : `unable to load SSL private key` | Clé chiffrée, illisible par l'UID 99, ou qui ne correspond pas au certificat (voir [docs/HAPROXY.md](docs/HAPROXY.md#problèmes-courants)). |
| Logs de l'API : `Permission denied (publickey…)` vers un hyperviseur | La clé SSH n'est pas autorisée sur l'hyperviseur, ou `SSH_PRIVATE_KEY_HOST_PATH` est faux. |
| Logs de l'API : `Host key verification failed` | L'hyperviseur n'est pas dans `~/.ssh/known_hosts` de la machine Docker : connectez-vous-y une fois en SSH. |
| La console noVNC ne s'ouvre pas | `NOVNC_URL` n'est pas `https://<serveur>:6443/vnc.html`, ou le certificat n'a pas été accepté sur le port 6443. |
| `port is already allocated` au démarrage | Un autre service utilise ce port (souvent le frontend, sur 80 / 443) : changez `API_*_PORT` ou `NOVNC_*_PORT`. |
| `429 Too Many Requests` à la connexion | Trop d'essais : 20 requêtes / 10 s par IP (HAProxy), ou 5 mots de passe faux pour un compte (bloqué 15 minutes). |

---

## Préparer un hyperviseur

Pour chaque hyperviseur distant, joint en `qemu+ssh://utilisateur@adresse/system` :

1. **KVM et libvirt installés**, `libvirtd` démarré (voir les paquets de l'option 1).
2. **Un utilisateur qui peut parler à libvirt** : `root`, ou un utilisateur du groupe
   `libvirt`.
3. **La clé SSH de l'API autorisée** pour cet utilisateur :

   ```bash
   ssh-copy-id -i ~/.ssh/id_ed25519.pub utilisateur@adresse
   ```

4. **L'hyperviseur connu** de la machine qui fait tourner l'API (une première connexion
   l'ajoute à `~/.ssh/known_hosts`), et la connexion qui marche sans mot de passe :

   ```bash
   ssh utilisateur@adresse virsh -c qemu:///system list --all
   ```

5. **Un pool de stockage et un réseau actifs** (en général `default`) :

   ```bash
   virsh -c qemu:///system pool-list --all
   virsh -c qemu:///system net-list --all
   ```

6. **Pour construire des templates** : `packer`, `qemu-img`, `virsh` et, pour les images
   cloud-init, `xorriso` sur l'hyperviseur. Packer y est lancé en SSH. Pour un hyperviseur
   local (`qemu:///system`), Packer tourne là où tourne l'API.

7. **Pour les consoles** : le port VNC des VM (5900-5999) doit être joignable depuis le
   conteneur noVNC. Ce port n'a pas de mot de passe : filtrez-le pour qu'il ne soit
   joignable **que** depuis la machine Docker, et réglez `VNC_LISTEN` sur l'adresse de
   l'hyperviseur qu'elle utilise.

---

Pour aller plus loin : [README.md](README.md) (vue d'ensemble), [docs/DOCKER.md](docs/DOCKER.md)
(API et frontend dans Docker), [docs/API.md](docs/API.md)
(routes), [docs/AUTH.md](docs/AUTH.md) (comptes et droits), [docs/HAPROXY.md](docs/HAPROXY.md)
(HTTPS), [docs/BRUNO.md](docs/BRUNO.md) (collection de requêtes).
