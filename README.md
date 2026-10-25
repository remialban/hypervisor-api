# KVM Multi-Hypervisor API

API Rust avec Axum pour enregistrer plusieurs hyperviseurs KVM et gerer leurs ressources.

**Pour installer et lancer le projet (développement ou Docker), suivez
[INSTALLATION.md](INSTALLATION.md).**

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

Au premier démarrage, l'API crée un administrateur si la base est vide. Définissez
son mot de passe (sinon il est généré et affiché une seule fois dans la console) :

```bash
ADMIN_PASSWORD='un mot de passe' JWT_SECRET="$(openssl rand -base64 48)" NOVNC_HOST=192.168.122.100 cargo run
```

Sans `JWT_SECRET`, une clé aléatoire est utilisée : les sessions ne survivent pas
à un redémarrage. Voir [Authentification](#authentification).

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

Le fichier `vm.cfg` (variable `VM_CONFIG_PATH` côté API) contient une ligne par
console ouverte, au format attendu par `websockify` :

```text
JETON_ALEATOIRE: ADRESSE_HYPERVISEUR:PORT_VNC
```

Chaque jeton est aléatoire et expire au bout de deux minutes (il disparaît alors
du fichier). Le fichier est régénéré lorsqu'une requête, faite par quelqu'un qui a
le droit d'accéder à la VM, est envoyée à :

```text
GET /api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc
```

L'endpoint renvoie ensuite une propriété `novnc_url` contenant le token de la
VM. Ouvrez cette URL dans le navigateur pour établir la connexion VNC.

L'ordre recommandé est :

1. démarrer l'API avec `NOVNC_HOST` ;
2. démarrer `websockify` dans un autre terminal ;
3. appeler l'endpoint VNC pour générer un jeton et sa ligne dans `vm.cfg` ;
4. ouvrir la valeur `novnc_url` retournée par l'API.

Exemple d'appel de l'endpoint VNC :

```bash
curl http://127.0.0.1:3000/api/hypervisors/<HYPERVISOR_ID>/vms/<VM_UUID>/vnc \
  -H "Authorization: Bearer <access_token>"
```

Arreter l'API ou `websockify` avec `Ctrl+C`.

### Lancer l'API et noVNC avec Docker Compose

Le Compose lance trois conteneurs :

- `haproxy` : la seule porte d'entrée, en HTTPS (port `443`, le port `80` redirige
  vers HTTPS). Il envoie `/api/…`, `/swagger-ui…`, `/api-docs/…` et `/ping` à l'API,
  et tout le reste à noVNC ;
- `api` : l'API Rust. Son port `3000` n'est ouvert que sur la machine elle-même
  (`127.0.0.1:3000`, pour Bruno ou curl en local) ;
- `novnc` : noVNC et websockify. Son port `6080` n'est pas publié du tout.

Le détail (certificat, en-têtes, limite sur l'authentification, dépannage) est dans
[docs/HAPROXY.md](docs/HAPROXY.md).

L'API garde sa base SQLite (`api.db`) dans `./data`. L'API et noVNC partagent en
plus un volume nommé `vnc-tokens` qui ne contient que le fichier de jetons
`vm.cfg` : l'API l'écrit, websockify le relit à chaque connexion (en lecture
seule). noVNC ne voit donc jamais la base ni les comptes. Les jetons noVNC sont
aléatoires et expirent au bout de deux minutes.

HAProxy a besoin d'un certificat et de sa clé privée, en PEM, dans **deux fichiers**
(la clé lisible par l'UID 99). Pour un essai, un certificat auto-signé suffit :

```bash
mkdir -p tls
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 365 \
  -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost" \
  -keyout tls/key.pem -out tls/cert.pem
sudo chown 99:99 tls/key.pem && sudo chmod 400 tls/key.pem
```

Copiez `.env.example` en `.env` et adaptez-le. `JWT_SECRET`, `ADMIN_PASSWORD`,
`TLS_CERT_PATH` et `TLS_KEY_PATH` sont obligatoires (Compose refuse de démarrer sans
eux) :

```dotenv
SSH_PRIVATE_KEY_HOST_PATH=/home/user/.ssh/id_ed25519
TLS_CERT_PATH=./tls/cert.pem
TLS_KEY_PATH=./tls/key.pem
HTTP_PORT=80
HTTPS_PORT=443
NOVNC_URL=https://localhost/vnc.html
DOCKERHUB_USERNAME=remi
TAG=latest
JWT_SECRET=<au moins 32 caractères aléatoires, par exemple : openssl rand -base64 48>
ADMIN_USERNAME=admin
ADMIN_PASSWORD=<mot de passe du premier administrateur>
CORS_ALLOWED_ORIGINS=
```

`ADMIN_USERNAME` / `ADMIN_PASSWORD` ne servent qu'au premier démarrage, quand la
base ne contient encore aucun compte. `CORS_ALLOWED_ORIGINS` liste les origines du
front Angular autorisées à appeler l'API depuis un navigateur (en `https://`) ;
laissez-le vide si le front est servi par la même adresse que l'API. Voir
[docs/AUTH.md](docs/AUTH.md) pour toutes les variables.

`DOCKERHUB_USERNAME` et `TAG` désignent les images publiées sur Docker Hub
(`<utilisateur>/hypervisor-api:<tag>`). **Ajoutez toujours `--build`** tant que
les images publiées ne contiennent pas l'authentification : sans lui, Compose
utilise une image déjà présente ou téléchargée, qui peut être antérieure à ces
changements et ignorerait `JWT_SECRET` et `ADMIN_PASSWORD`.

Le Compose monte aussi `~/.ssh/known_hosts` de l'hôte dans l'API (pour les
connexions `qemu+ssh`) : ce fichier doit exister.

Puis lancez l'application :

```bash
docker compose up --build --remove-orphans
```

L'API est disponible sur `https://localhost/api/…`, Swagger sur
`https://localhost/swagger-ui/` et noVNC sur `https://localhost/vnc.html`.
Connectez-vous avec le compte administrateur (bouton **Authorize** de Swagger, ou
`POST /api/auth/login`). Avec le certificat auto-signé, acceptez l'avertissement du
navigateur une fois, et ajoutez `--cacert tls/cert.pem` à curl.

Sauvegardez `./data/api.db` : il contient les comptes, les droits, les
propriétaires de VM et le journal d'audit. Changer `JWT_SECRET` déconnecte tout
le monde mais ne perd rien.

`NOVNC_URL` est l'URL de base utilisée par l'API pour construire la propriété
`novnc_url` retournée par l'endpoint VNC. L'API ajoute automatiquement le
token et `autoconnect=true`. Elle doit être l'adresse HTTPS de HAProxy, avec le
nom du certificat : noVNC ouvre alors la console en `wss://` (chiffré). Si Docker
tourne sur une machine joignable à `kvm.example.com` :

```dotenv
NOVNC_URL=https://kvm.example.com/vnc.html
```

HAProxy chiffre le trajet entre le navigateur et le serveur, mais le port VNC des VM
sur l'hyperviseur reste ouvert sur le réseau : filtrez-le (voir la fin de
[docs/HAPROXY.md](docs/HAPROXY.md)).

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

Les tables sont créées par les migrations du dossier `migrations/`, appliquées
automatiquement au démarrage. Elles contiennent :

- `hypervisors` : les hyperviseurs enregistrés ;
- `users`, `refresh_tokens` : comptes (mots de passe hachés en Argon2id), quotas, sessions ;
- `hypervisor_access` : qui peut créer des VM sur quel hyperviseur ;
- `vm_owners`, `vm_shares` : propriétaire de chaque VM et prêts en cours ;
- `settings`, `vnc_tokens`, `audit_log` : réglages, jetons noVNC, journal.

Les VM, disques et réseaux ne sont pas dans SQLite : leur source de vérité est
libvirt. Seuls leur propriétaire et leurs prêts sont gardés ici, par UUID de VM.

Ne pas supprimer `api.db` si les donnees doivent etre conservees.

## Authentification

Toutes les routes, sauf `/ping` et `/api/auth/*`, demandent un jeton
`Authorization: Bearer <access_token>`.

```bash
# Se connecter (JWT de 15 minutes + refresh token à usage unique)
curl -X POST http://127.0.0.1:3000/api/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"<ADMIN_PASSWORD>"}'

# Utiliser le jeton
curl http://127.0.0.1:3000/api/hypervisors -H "Authorization: Bearer <access_token>"
```

`POST /api/auth/token` accepte aussi le formulaire OAuth2 standard
(`grant_type=password` ou `refresh_token`), ce qu'utilise le bouton **Authorize**
de Swagger.

Deux rôles : `admin` (tout) et `user`. Un `user` ne crée des VM que sur les
hyperviseurs où un admin lui a donné accès, ne voit que ses VM et celles qu'on lui
prête (console + démarrer/arrêter), et peut être soumis à des quotas. Les comptes
sont créés par un admin ; l'inscription libre existe mais reste fermée tant qu'un
admin ne l'ouvre pas. Le détail (matrice des droits, prêt de VM, quotas, journal
d'audit, branchement du front Angular) est dans [docs/AUTH.md](docs/AUTH.md).

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

Se connecter et garder le jeton (voir [Authentification](#authentification)) :

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:3000/api/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"<ADMIN_PASSWORD>"}' | jq -r .access_token)
```

Ajouter un hyperviseur local :

```bash
curl -X POST http://127.0.0.1:3000/api/hypervisors \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"kvm-local","uri":"qemu:///system"}'
```

Lister les hyperviseurs :

```bash
curl http://127.0.0.1:3000/api/hypervisors -H "Authorization: Bearer $TOKEN"
```

L'URI libvirt doit etre joignable. Une URI invalide retourne `502 Bad Gateway`.

## Organisation du code

```text
src/
├── main.rs            Demarrage Axum, SQLite et Swagger
├── state.rs           Pool SQLite, migrations et etat partage
├── config.rs          Variables d'environnement (JWT, durees, CORS...)
├── auth.rs            JWT, mots de passe Argon2, extracteur AuthUser
├── users.rs           Login, comptes, quotas, reglages
├── access.rs          Droits : roles, proprietaire de VM, acces hyperviseur, quotas
├── shares.rs          Pret de VM et changement de proprietaire
├── audit.rs           Journal d'audit
├── models.rs          Modeles JSON et SQLx
├── routes.rs          Declaration des routes
├── hypervisors.rs     CRUD hyperviseurs et controle libvirt
├── vms.rs             Routes VM, sans stockage local
├── infrastructure.rs  Routes disques et reseaux, sans stockage local
├── vnc.rs             Jetons noVNC courts et fichier lu par websockify
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
