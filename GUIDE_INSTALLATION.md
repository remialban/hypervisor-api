# Guide d'installation rapide

Lancer l'API, les consoles noVNC et l'interface web (`hypervisor-ui`) avec Docker, sur une
seule machine. Pour le détail, voir [INSTALLATION.md](INSTALLATION.md).

**Il faut :** Docker avec Docker Compose, `git` et `openssl`.

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

## 3. Lancer l'API et noVNC

```bash
cp .env.example .env
```

Dans `.env`, changez ces lignes :

```dotenv
SSH_PRIVATE_KEY_HOST_PATH=/home/<vous>/.ssh/id_ed25519
TLS_CERT_PATH=./tls/cert.pem
TLS_KEY_PATH=./tls/key.pem
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

## Arrêter

```bash
cd hypervisor-ui && docker compose down
cd ../hypervisor-api && docker compose down
```

Les données (comptes, droits, VM…) restent dans `hypervisor-api/data/`.

## Si ça ne marche pas

| Message | Solution |
|---|---|
| `bind source path does not exist` | Un chemin du `.env` est faux (clé SSH ou certificat). |
| `network … not found` (interface) | Lancez l'API d'abord. |
| `port is already allocated` | Un autre programme utilise déjà le port 80, 443, 8443 ou 6443. |
| Erreur `502` dans l'application | L'API n'est pas lancée : `docker compose ps` dans `hypervisor-api`. |

Les autres cas sont dans [INSTALLATION.md](INSTALLATION.md#10-problèmes-courants).
