# Guide d'installation rapide

Lancer l'API, les consoles noVNC et l'interface web (`hypervisor-ui`) avec Docker, sur une
seule machine. Pour le détail, voir [INSTALLATION.md](INSTALLATION.md).

**Il faut :** Docker avec Docker Compose, `git` et `openssl`. Pour les templates, Packer sur
l'hyperviseur (voir [plus bas](#templates--installer-packer)).

## 1. Récupérer les deux projets

Dans un même dossier, côte à côte :

```bash
git clone git@github.com:remialban/hypervisor-api.git
git clone git@github.com:tkdev1755/hypervisor-ui.git
```

## 2. Créer le certificat HTTPS

Un seul certificat sert à tout :

```bash
cd hypervisor-api
mkdir -p tls && openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -days 365 -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  -keyout tls/key.pem -out tls/cert.pem && sudo chown 99:99 tls/key.pem && sudo chmod 400 tls/key.pem
```

Pour ouvrir l'application depuis une autre machine, ajoutez son adresse au certificat, par
exemple `IP:192.168.1.10` après `IP:127.0.0.1`.

## 3. Lancer l'API, noVNC et la base

```bash
cp .env.example .env
```

Dans `.env`, changez ces lignes :

```dotenv
SSH_PRIVATE_KEY_HOST_PATH=/home/<vous>/.ssh/id_ed25519
TLS_CERT_PATH=./tls/cert.pem
TLS_KEY_PATH=./tls/key.pem
POSTGRES_PASSWORD=<lettres et chiffres au hasard>
JWT_SECRET=<au moins 32 caractères au hasard>
ADMIN_PASSWORD=<mot de passe de l'administrateur>
```

Puis :

```bash
docker compose up -d --build
```

## 4. Lancer l'interface web

```bash
cd ../hypervisor-ui
cp .env.example .env
```

Dans `.env`, changez ces deux lignes, pour réutiliser le certificat de l'API :

```dotenv
TLS_CERT_PATH=../hypervisor-api/tls/cert.pem
TLS_KEY_PATH=../hypervisor-api/tls/key.pem
```

Puis :

```bash
docker compose up -d --build
```

L'API doit être lancée **avant** l'interface : l'interface se branche sur son réseau.

## 5. Utiliser

| Adresse | Pour quoi |
|---|---|
| <https://localhost> | **l'application** : connexion avec `admin` et votre `ADMIN_PASSWORD` |
| <https://localhost:6443/vnc.html> | les consoles des VM |
| <https://localhost:8443/swagger-ui/> | Swagger, pour tester l'API |

Le certificat est auto-signé : le navigateur affiche un avertissement. **Acceptez-le une fois
sur chacune des trois adresses**, sinon les consoles restent vides.

Ensuite, dans l'application, ajoutez un hyperviseur : `qemu:///system` pour cette machine,
ou `qemu+ssh://utilisateur@adresse/system` pour un hyperviseur distant (votre clé SSH doit
y être autorisée).

## Templates : installer Packer

Les templates sont construits par **Packer**, que l'image Docker de l'API ne contient pas.
Installez-le **sur l'hyperviseur** (avec `qemu-img`, `virsh` et `xorriso`) : l'API l'y lance
en SSH. Un hyperviseur ajouté en `qemu:///system` ne peut donc pas construire de templates ;
pour construire sur cette machine, ajoutez-la plutôt en `qemu+ssh://utilisateur@<son adresse>/system`.

Installation et vérification : [INSTALLATION.md](INSTALLATION.md#installer-packer).

## Arrêter

```bash
cd hypervisor-ui && docker compose down
cd ../hypervisor-api && docker compose down
```

Les données (comptes, droits, VM…) restent dans la base PostgreSQL (volume Docker `db-data`).
N'utilisez pas `docker compose down -v` : il l'effacerait.

Vous aviez déjà une installation avec l'ancienne base SQLite (`data/api.db`) ? Reprenez ses
données avec `scripts/sqlite-to-postgres.sh` (voir [INSTALLATION.md](INSTALLATION.md#9-exploitation)).

## Si ça ne marche pas

| Message | Solution |
|---|---|
| `bind source path does not exist` | Un chemin du `.env` est faux (clé SSH ou certificat). |
| `network … not found` (interface) | Lancez l'API d'abord. |
| `port is already allocated` | Un autre programme utilise déjà le port 80, 443, 8443 ou 6443. |
| Erreur `502` dans l'application | L'API n'est pas lancée : `docker compose ps` dans `hypervisor-api`. |

Les autres cas sont dans [INSTALLATION.md](INSTALLATION.md#10-problèmes-courants).
