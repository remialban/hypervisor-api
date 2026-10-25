# Installation

Ce guide explique comment faire tourner l'API, de deux façons :

- **[Option 1 — Développement](#option-1--développement)** : l'API lancée avec `cargo run`
  sur votre machine, pour modifier le code et le tester ;
- **[Option 2 — Installation avec Docker](#option-2--installation-avec-docker)** : toute
  l'application avec Docker Compose (l'API, les consoles noVNC et le frontend `hypervisor-ui`,
  derrière trois HAProxy en HTTPS), pour l'utiliser au quotidien ou la déployer sur un
  serveur.

Dans les deux cas, l'API pilote des hyperviseurs KVM via libvirt. La
[dernière partie](#préparer-un-hyperviseur) explique comment préparer un hyperviseur.

---

## Option 1 — Développement

### 1. Prérequis

- Linux avec KVM et libvirt.
- Rust 1.88 ou plus récent (édition 2024 et `if let … &&`) : <https://rustup.rs>.
- Les en-têtes de libvirt et `pkg-config`, pour compiler la crate `virt`.
- **PostgreSQL**, pour la base de l'API. Le plus simple est de le lancer avec Docker
  (étape 2).

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

L'API a besoin d'une base PostgreSQL. Pour le développement, lancez-en une avec Docker :
elle sert aussi aux tests (étape 5).

```bash
docker run -d --name hypervisor-dev-pg --restart unless-stopped \
  -e POSTGRES_PASSWORD=dev -e POSTGRES_DB=hypervisor \
  -p 127.0.0.1:55432:5432 postgres:17-alpine -c max_connections=500
docker exec hypervisor-dev-pg createdb -U postgres hypervisor_test   # base des tests
```

Puis, depuis la racine du projet :

```bash
DATABASE_URL=postgres://postgres:dev@127.0.0.1:55432/hypervisor \
ADMIN_PASSWORD='un-mot-de-passe' \
JWT_SECRET="$(openssl rand -base64 48)" \
cargo run
```

L'API écoute sur `http://127.0.0.1:3000`. Au premier démarrage, les tables sont créées
(migrations de `migrations/`), et un administrateur `admin` est créé avec `ADMIN_PASSWORD`. Sans `ADMIN_PASSWORD`, un mot de passe est généré et affiché
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
(`test:///default`). Ils ont en revanche besoin de PostgreSQL : chaque test crée son
propre schéma vide dans la base `TEST_DATABASE_URL`. Par défaut, c'est
`postgres://postgres:test@127.0.0.1:55432/hypervisor_test`. Avec le conteneur de l'étape 2,
indiquez son mot de passe :

```bash
export TEST_DATABASE_URL=postgres://postgres:dev@127.0.0.1:55432/hypervisor_test
```

Cette base est jetable : les tests y laissent un schéma `test_…` chacun. Pour la vider :
`docker exec hypervisor-dev-pg dropdb -U postgres hypervisor_test`, puis la recréer.

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
| `DATABASE_URL` | — (obligatoire) | Base PostgreSQL : `postgres://utilisateur:motdepasse@hôte:5432/base` |
| `TEST_DATABASE_URL` | `postgres://postgres:test@127.0.0.1:55432/hypervisor_test` | Base jetable des tests (`cargo test`) |
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

Cette option installe **toute l'application** sur une machine : l'API, les consoles noVNC
et le frontend Angular. Elle utilise deux dépôts, chacun avec son `docker-compose.yml` :

- `hypervisor-api` (ce dépôt) : l'API et noVNC ;
- `hypervisor-ui` : le frontend.

Au total, sept conteneurs tournent :

| Conteneur | Dépôt | Rôle | Ports sur la machine |
|---|---|---|---|
| `hypervisor-ui-haproxy` | ui | Entrée HTTPS de **l'application** | `443` (HTTPS), `80` (redirigé) |
| `hypervisor-ui` | ui | nginx : sert l'application, relaie `/api` vers `api-haproxy` | aucun |
| `hypervisor-api-haproxy` | api | Entrée HTTPS de l'API : pour le front (interne `:9443`), Swagger, Bruno | `8443` (HTTPS), `8081` (redirigé) |
| `hypervisor-api` | api | L'API Rust | `127.0.0.1:3000` seulement |
| `hypervisor-novnc-haproxy` | api | Entrée HTTPS des **consoles** | `6443` (HTTPS), `6081` (redirigé) |
| `hypervisor-novnc` | api | noVNC et websockify | aucun |
| `hypervisor-db` | api | La base **PostgreSQL** de l'API, sur un réseau que seule l'API rejoint | aucun |

Le fonctionnement détaillé (réseaux, volumes, chemin des requêtes, schémas) est dans
[docs/DOCKER.md](docs/DOCKER.md), et celui des HAProxy dans [docs/HAPROXY.md](docs/HAPROXY.md).

### 1. Prérequis

- Docker Engine et le plugin Docker Compose v2.24 ou plus récent (`docker compose version`).
- `git` et `openssl`.
- Les ports **80, 443, 8081, 8443, 6081 et 6443** libres sur la machine.
- Une clé SSH qui ouvre une session sur vos hyperviseurs, et un fichier `~/.ssh/known_hosts`
  qui les connaît (voir [Préparer un hyperviseur](#préparer-un-hyperviseur)).
- Pour un hyperviseur local : libvirt installé sur la machine Docker (le socket
  `/var/run/libvirt/libvirt-sock` est monté dans l'API).

### 2. Récupérer les deux dépôts

Clonez-les **côte à côte**, dans le même dossier, en gardant leur nom :

```bash
mkdir -p ~/hypervisor && cd ~/hypervisor
git clone git@github.com:remialban/hypervisor-api.git
git clone git@github.com:tkdev1755/hypervisor-ui.git
```

```text
~/hypervisor/
├── hypervisor-api/    API + noVNC
└── hypervisor-ui/     frontend
```

Le nom du dossier `hypervisor-api` compte : Docker en tire le nom du réseau que le front
rejoint (`hypervisor-api_hypervisor-net`). Avec un autre nom, changez `API_NETWORK` à
l'étape 5.

### 3. Le certificat HTTPS

Un seul certificat sert aux trois HAProxy. Il est aussi celui que nginx vérifie quand il
appelle l'API. Créez-le une fois, dans `hypervisor-api/tls/`.

**Pour un essai ou un réseau local**, un certificat auto-signé suffit. Mettez dans
`subjectAltName` **`localhost`**, plus chaque nom et adresse par lesquels les navigateurs
joindront le serveur (ici `kvm.local` et `192.168.1.10`, à adapter) :

```bash
cd ~/hypervisor/hypervisor-api
mkdir -p tls
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 365 \
  -subj "/CN=kvm.local" \
  -addext "subjectAltName=DNS:localhost,DNS:kvm.local,IP:127.0.0.1,IP:192.168.1.10" \
  -keyout tls/key.pem -out tls/cert.pem
sudo chown 99:99 tls/key.pem && sudo chmod 400 tls/key.pem
chmod 644 tls/cert.pem
```

- La clé doit être lisible par l'UID 99 : c'est l'utilisateur de HAProxy dans les conteneurs.
- `tls/` est ignoré par git et par Docker : la clé ne finit ni dans un commit ni dans une
  image.
- Pour la supprimer plus tard, il faudra `sudo`.

**Pour un accès public**, utilisez un vrai certificat (Let's Encrypt…) : voir
[docs/HAPROXY.md](docs/HAPROXY.md#avec-un-certificat-existant-lets-encrypt-autorité-interne).

### 4. Configurer l'API et noVNC

```bash
cd ~/hypervisor/hypervisor-api
cp .env.example .env
```

Puis éditez `.env` et **remplacez toutes les valeurs d'exemple**. Un chemin d'exemple laissé
tel quel (`/home/user/…`, `/etc/hypervisor-api/…`) fait échouer le démarrage avec
`bind source path does not exist`.

```dotenv
# Images (laissez tel quel : elles sont construites avec --build)
DOCKERHUB_USERNAME=remi
TAG=latest

# Clé SSH vers les hyperviseurs : le chemin réel, sur cette machine
SSH_PRIVATE_KEY_HOST_PATH=/home/<vous>/.ssh/id_ed25519

# Certificat créé à l'étape 3
TLS_CERT_PATH=./tls/cert.pem
TLS_KEY_PATH=./tls/key.pem

# Ports des HAProxy de l'API et de noVNC (80 et 443 sont pour le frontend)
API_HTTP_PORT=8081
API_HTTPS_PORT=8443
NOVNC_HTTP_PORT=6081
NOVNC_HTTPS_PORT=6443

# Adresse des consoles vue par les navigateurs : le HAProxy de noVNC (port 6443),
# avec un nom ou une IP présent dans le certificat
NOVNC_URL=https://192.168.1.10:6443/vnc.html

# Base PostgreSQL : lettres et chiffres seulement (le mot de passe est mis dans une URL)
POSTGRES_PASSWORD=<résultat de : openssl rand -hex 24>

# Secrets : à générer, jamais les valeurs d'exemple
JWT_SECRET=<résultat de : openssl rand -base64 48>
ADMIN_USERNAME=admin
ADMIN_PASSWORD=<mot de passe du premier administrateur>

# Vide : le frontend appelle l'API par son propre nginx, sur sa propre origine
CORS_ALLOWED_ORIGINS=
```

`ADMIN_USERNAME` et `ADMIN_PASSWORD` ne servent qu'au tout premier démarrage, quand la base
est vide. Changez ensuite le mot de passe depuis l'application (ou `PUT /api/me/password`).

### 5. Configurer le frontend

```bash
cd ~/hypervisor/hypervisor-ui
cp .env.example .env
```

Puis éditez `.env`. Le front réutilise le certificat de l'étape 3 :

```dotenv
# Image du front (construite avec --build)
TAG=latest

# HTTPS de l'application : le même certificat que l'API
TLS_CERT_PATH=../hypervisor-api/tls/cert.pem
TLS_KEY_PATH=../hypervisor-api/tls/key.pem
HTTP_PORT=80
HTTPS_PORT=443

# Lien avec l'API
API_NETWORK=hypervisor-api_hypervisor-net
API_UPSTREAM=https://api-haproxy:9443
# Ce à quoi nginx fait confiance pour le certificat de l'API, et le nom qu'il y vérifie.
# Auto-signé : le certificat lui-même, et localhost (présent dans le certificat).
# Let's Encrypt : /etc/ssl/certs/ca-certificates.crt, et votre domaine.
API_CA_PATH=../hypervisor-api/tls/cert.pem
API_TLS_NAME=localhost
```

Les variables `MOCK_FULL` et `PUBLIC_URL` ne servent qu'à la fausse API : laissez-les.

### 6. Démarrer

**L'ordre compte** : l'API d'abord. Elle crée le réseau que le front rejoint, et nginx a
besoin de `api-haproxy` pour démarrer.

```bash
# 1. L'API et noVNC
cd ~/hypervisor/hypervisor-api
docker compose up -d --build --remove-orphans

# 2. Le frontend
cd ~/hypervisor/hypervisor-ui
docker compose up -d --build

# Les sept conteneurs doivent être « Up »
docker ps --format 'table {{.Names}}\t{{.Status}}\t{{.Ports}}'
```

La première construction prend plusieurs minutes (compilation Rust et Angular). `--build`
construit les images depuis le code ; sans lui, Compose peut prendre une image plus ancienne.

### 7. Vérifier

Depuis la machine (`--cacert` n'est utile qu'avec le certificat auto-signé) :

```bash
cd ~/hypervisor/hypervisor-api
curl --cacert tls/cert.pem https://localhost/ping          # application → API : {"status":"ok",…}
curl --cacert tls/cert.pem https://localhost:8443/ping     # API en direct : {"status":"ok",…}
curl --cacert tls/cert.pem -o /dev/null -w '%{http_code}\n' https://localhost:6443/vnc.html   # 200
curl -I http://localhost/                                  # 301 vers https://
```

Puis dans le navigateur, avec le certificat auto-signé, **acceptez l'avertissement sur les
trois adresses**, une fois chacune. Chaque port est une origine différente, et une console
reste vide tant que le certificat n'a pas été accepté sur le port 6443.

| Adresse | Ce qu'on y trouve |
|---|---|
| `https://<serveur>` | **l'application** |
| `https://<serveur>:6443/vnc.html` | noVNC (les consoles s'ouvrent depuis l'application) |
| `https://<serveur>:8443/swagger-ui/` | Swagger, pour tester l'API |

Pour ne plus avoir d'avertissement, importez `hypervisor-api/tls/cert.pem` comme autorité de
confiance dans le navigateur ou le système.

### 8. Premiers pas

1. Ouvrez `https://<serveur>` et connectez-vous avec `ADMIN_USERNAME` / `ADMIN_PASSWORD`.
2. Changez le mot de passe de l'administrateur.
3. Ajoutez un hyperviseur : `qemu:///system` pour la machine elle-même, ou
   `qemu+ssh://utilisateur@adresse/system` pour un hyperviseur distant (voir
   [Préparer un hyperviseur](#préparer-un-hyperviseur)).
4. Créez des comptes et des groupes, donnez-leur accès aux hyperviseurs ou aux templates.
5. Ouvrez la console d'une VM : elle s'affiche par noVNC, sur le port 6443.

### 9. Exploitation

**Logs** :

```bash
cd ~/hypervisor/hypervisor-api
docker compose logs -f api                          # l'API
docker compose logs -f api-haproxy novnc-haproxy    # une ligne par requête, avec le code
cd ~/hypervisor/hypervisor-ui
docker compose logs -f                              # HAProxy et nginx du front
```

**Changer de certificat** : remplacez les fichiers, puis redémarrez les trois HAProxy, ainsi
que nginx qui vérifie le certificat de l'API :

```bash
(cd ~/hypervisor/hypervisor-api && docker compose restart api-haproxy novnc-haproxy)
(cd ~/hypervisor/hypervisor-ui && docker compose restart)
```

**Arrêter** : dans l'ordre inverse du démarrage. Docker refuse de supprimer le réseau de
l'API tant que le front y est branché.

```bash
(cd ~/hypervisor/hypervisor-ui && docker compose down)
(cd ~/hypervisor/hypervisor-api && docker compose down)
```

Les données restent : `down` ne supprime pas les volumes (base `db-data`, jetons). En
revanche, **`docker compose down -v` efface la base** : ne l'utilisez pas sans sauvegarde.

**Mettre à jour** :

```bash
(cd ~/hypervisor/hypervisor-api && git pull && docker compose up -d --build --remove-orphans)
(cd ~/hypervisor/hypervisor-ui && git pull && docker compose up -d --build)
```

Les migrations de la base s'appliquent toutes seules au démarrage de l'API.

**Sauvegarder** : la base PostgreSQL contient les comptes, les groupes, les droits, les
propriétaires des VM, les templates et le journal d'audit. Les VM et leurs disques sont,
eux, sur les hyperviseurs. Le front ne garde aucune donnée.

```bash
cd ~/hypervisor/hypervisor-api
docker compose exec -T db pg_dump -U hypervisor -Fc hypervisor > sauvegarde.dump      # sauvegarder
docker compose exec -T db pg_restore -U hypervisor -d hypervisor --clean < sauvegarde.dump   # restaurer
```

Changer `JWT_SECRET` déconnecte tout le monde mais ne perd rien.

**Reprendre les données d'une ancienne installation SQLite** (`data/api.db`) : démarrez la
stack une fois sur PostgreSQL (l'API crée les tables), puis, dans `hypervisor-api` :

```bash
scripts/sqlite-to-postgres.sh data/api.db
```

Le script demande confirmation, arrête l'API, **vide** la base PostgreSQL, y copie tout le
contenu de `api.db` (comptes, hyperviseurs, droits, groupes, templates, journal…) et
redémarre l'API. Le fichier SQLite n'est que lu. Il faut `sqlite3` sur la machine.

### 10. Problèmes courants

| Symptôme | Cause et solution |
|---|---|
| `bind source path does not exist` | Un chemin d'un `.env` (`TLS_*`, `API_CA_PATH`, `SSH_PRIVATE_KEY_HOST_PATH`) n'existe pas. Les chemins relatifs partent du dossier du `docker-compose.yml`. |
| Front : `network hypervisor-api_hypervisor-net … not found` | L'API n'est pas lancée, ou son dossier ne s'appelle pas `hypervisor-api` : lancez-la d'abord, ou corrigez `API_NETWORK`. |
| Front : `host not found in upstream "api-haproxy"`, nginx redémarre en boucle | `hypervisor-api-haproxy` n'est pas lancé : démarrez l'API, puis `docker compose restart` dans `hypervisor-ui`. |
| L'application affiche une erreur `502` | nginx ne joint pas l'API. Avec `upstream SSL certificate verify error` dans `docker compose logs ui` : `API_TLS_NAME` n'est pas dans le certificat, ou `API_CA_PATH` n'est pas le bon fichier. Sinon : `hypervisor-api-haproxy` ou `hypervisor-api` est arrêté. |
| `checking context: no permission to read …` pendant le build | Un fichier illisible est dans le dossier du projet hors de `tls/` : déplacez-le, ou ajoutez-le à `.dockerignore`. |
| Un HAProxy redémarre en boucle : `no SSL certificate specified` | Le chemin du certificat désigne un dossier, pas un fichier. |
| HAProxy : `unable to load SSL private key` | Clé chiffrée, illisible par l'UID 99, ou qui ne correspond pas au certificat (voir [docs/HAPROXY.md](docs/HAPROXY.md#problèmes-courants)). |
| `port is already allocated` | Un autre service utilise déjà ce port : libérez-le, ou changez `HTTP_PORT` / `HTTPS_PORT` (front), `API_*_PORT`, `NOVNC_*_PORT` (API). |
| Logs de l'API : `Permission denied (publickey…)` vers un hyperviseur | La clé SSH n'est pas autorisée sur l'hyperviseur, ou `SSH_PRIVATE_KEY_HOST_PATH` est faux. |
| Logs de l'API : `Host key verification failed` | L'hyperviseur n'est pas dans `~/.ssh/known_hosts` de la machine Docker : connectez-vous-y une fois en SSH. |
| La console reste vide ou ne se connecte pas | Le certificat n'a pas été accepté sur `https://<serveur>:6443`, ou `NOVNC_URL` n'est pas `https://<serveur>:6443/vnc.html`. |
| L'API redémarre en boucle : `Could not initialize the PostgreSQL database` | La base n'est pas joignable, ou `POSTGRES_PASSWORD` a changé après sa création (le mot de passe est fixé au premier démarrage du volume `db-data`). |
| `POSTGRES_PASSWORD doit être défini` | Ajoutez `POSTGRES_PASSWORD` au `.env` de l'API. |
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
