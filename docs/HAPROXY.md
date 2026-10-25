# HTTPS avec HAProxy

Avec Docker Compose, l'API et noVNC ne sont joignables qu'en HTTPS, chacun par **son propre
HAProxy**. Chaque HAProxy chiffre la connexion avec votre certificat, puis transmet tout à
son conteneur :

| Conteneur HAProxy | HTTP (redirigé) | HTTPS | Envoie vers |
|---|---|---|---|
| `hypervisor-api-haproxy` | `API_HTTP_PORT` (8081) | `API_HTTPS_PORT` (8443), et `:8000` interne pour le front | `api:3000` : API, Swagger, OpenAPI |
| `hypervisor-novnc-haproxy` | `NOVNC_HTTP_PORT` (6081) | `NOVNC_HTTPS_PORT` (6443) | `novnc:6080` : pages noVNC et WebSocket des consoles |

Les ports 80 et 443 sont laissés au troisième HAProxy, celui du frontend (dépôt
`hypervisor-ui`), qui sert l'application. Son nginx relaie `/api` vers le HAProxy de
l'API, sur une entrée HTTP interne (`api-haproxy:8000`, non publiée) : toutes les requêtes
vers l'API passent donc par `hypervisor-api-haproxy`. Le schéma d'ensemble est dans [DOCKER.md](DOCKER.md).

Le port HTTP de chaque HAProxy ne sert qu'à rediriger vers son port HTTPS.

La configuration est découpée en trois fichiers :

- [haproxy/common.cfg](../haproxy/common.cfg) : ce qui est commun (TLS, délais, DNS de
  Docker, redirection HTTP → HTTPS) ;
- [haproxy/api.cfg](../haproxy/api.cfg) : l'entrée de l'API ;
- [haproxy/novnc.cfg](../haproxy/novnc.cfg) : l'entrée des consoles.

Chaque conteneur charge `common.cfg` puis son propre fichier (`command:` dans
`docker-compose.yml`). Les deux utilisent le même certificat.

## Ce que HAProxy apporte

- **Chiffrement** : TLS 1.2 et 1.3 uniquement, avec la liste de chiffrements « intermediate »
  de Mozilla. Les mots de passe, les jetons JWT, les jetons noVNC et tout ce qui passe par la
  console sont chiffrés entre le navigateur et le serveur.
- **Ports fermés** : noVNC n'est plus publié du tout. L'API n'est publiée que sur
  `127.0.0.1:3000`, donc seulement pour la machine elle-même (Bruno, curl). Pour la fermer
  aussi, supprimez la section `ports` du service `api` dans `docker-compose.yml`.
- **Limite sur l'authentification** (HAProxy de l'API) : au plus 20 requêtes sur
  `/api/auth/…` toutes les 10 secondes par adresse IP, puis `429`. Cette limite s'ajoute à
  celle de l'API, qui bloque un compte 15 minutes après 5 mots de passe faux. Le HAProxy du
  frontend applique la même limite ; les connexions faites par l'application passent par les
  deux (nginx transmet l'adresse du navigateur au HAProxy de l'API).
- **En-têtes de sécurité** :
  - `Strict-Transport-Security` : le navigateur n'utilisera plus que HTTPS pour ce site ;
  - `Referrer-Policy: no-referrer` (HAProxy de noVNC) : l'URL noVNC, qui contient le jeton,
    n'est jamais envoyée à un autre site ;
  - `X-Content-Type-Options: nosniff`.
- **Consoles longues** : une console WebSocket peut rester inactive jusqu'à une heure sans
  être coupée (`timeout tunnel`).

## Le certificat

Le certificat et la clé privée sont dans **deux fichiers PEM séparés** :

- `TLS_CERT_PATH` : le certificat du serveur, suivi des certificats intermédiaires (la
  chaîne), par exemple le `fullchain.pem` de Let's Encrypt ;
- `TLS_KEY_PATH` : la clé privée, non chiffrée (sans phrase de passe).

```dotenv
TLS_CERT_PATH=/etc/hypervisor-api/tls/cert.pem
TLS_KEY_PATH=/etc/hypervisor-api/tls/key.pem
```

Compose refuse de démarrer si l'une des deux variables manque. Dans le conteneur, HAProxy
lit le certificat dans `site.pem` et la clé à côté, dans `site.pem.key`.

HAProxy tourne avec l'utilisateur d'UID 99 dans son conteneur : la clé doit être lisible
par cet UID. Pour qu'elle ne soit lisible par personne d'autre :

```bash
sudo chown 99:99 /etc/hypervisor-api/tls/key.pem
sudo chmod 400 /etc/hypervisor-api/tls/key.pem
```

Le certificat est public : il peut rester lisible par tous (`chmod 644`).

### Avec un certificat existant (Let's Encrypt, autorité interne…)

Pointez directement vers les fichiers, sans les assembler :

```dotenv
TLS_CERT_PATH=/etc/letsencrypt/live/kvm.example.com/fullchain.pem
TLS_KEY_PATH=/etc/letsencrypt/live/kvm.example.com/privkey.pem
```

Docker suit les liens symboliques de `live/` au moment du montage. La clé de certbot n'est
lisible que par root : copiez-la plutôt, par exemple dans un hook `--deploy-hook`, puis
rechargez HAProxy après chaque renouvellement :

```bash
sudo install -o 99 -g 99 -m 400 /etc/letsencrypt/live/kvm.example.com/privkey.pem \
  /etc/hypervisor-api/tls/key.pem
sudo install -m 644 /etc/letsencrypt/live/kvm.example.com/fullchain.pem \
  /etc/hypervisor-api/tls/cert.pem
docker compose restart api-haproxy novnc-haproxy
```

### Certificat auto-signé (tests uniquement)

```bash
mkdir -p tls
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 365 \
  -subj "/CN=kvm.local" -addext "subjectAltName=DNS:kvm.local,IP:192.168.122.100" \
  -keyout tls/key.pem -out tls/cert.pem
sudo chown 99:99 tls/key.pem && sudo chmod 400 tls/key.pem
```

Puis, dans `.env` : `TLS_CERT_PATH=./tls/cert.pem` et `TLS_KEY_PATH=./tls/key.pem`. Le
dossier `tls/` est ignoré par git.

Mettez dans `subjectAltName` le nom et/ou l'IP par lesquels les navigateurs joindront le
serveur. Le navigateur affiche un avertissement **pour chaque port** (chaque port est une
origine différente) : acceptez-le sur `https://serveur:6443` avant d'ouvrir une console,
sinon la console reste vide. Importer `tls/cert.pem` comme autorité de confiance évite tous
ces avertissements.

## Configuration `.env`

```dotenv
# Certificat (+ chaîne) et clé privée, en PEM, dans deux fichiers.
TLS_CERT_PATH=/etc/hypervisor-api/tls/cert.pem
TLS_KEY_PATH=/etc/hypervisor-api/tls/key.pem

# Ports publiés sur l'hôte : HTTP (redirigé) et HTTPS de chaque HAProxy.
# 80 et 443 sont laissés au frontend.
API_HTTP_PORT=8081
API_HTTPS_PORT=8443
NOVNC_HTTP_PORT=6081
NOVNC_HTTPS_PORT=6443

# Adresse de noVNC vue par le navigateur : le HAProxy de noVNC, avec le nom du certificat.
NOVNC_URL=https://kvm.example.com:6443/vnc.html
```

`NOVNC_URL` doit être en `https://`, sur le port `NOVNC_HTTPS_PORT`, et utiliser le nom du
certificat. noVNC ouvre alors la console en `wss://` (WebSocket chiffré) sur la même adresse.

Le frontend appelle l'API par son propre nginx (qui relaie vers `api-haproxy:8000`), sur sa
propre origine : laissez
`CORS_ALLOWED_ORIGINS` vide. Ne le remplissez (en `https://`) que pour un front servi
ailleurs qui appellerait l'API directement sur le port 8443.

## Démarrage et vérification

```bash
docker compose up --build -d --remove-orphans
curl https://kvm.example.com:8443/ping              # {"status":"ok","content":"pong"}
curl -I http://kvm.example.com:8081/ping            # 301 vers https://…:8443
curl -o /dev/null -w '%{http_code}\n' https://kvm.example.com:6443/vnc.html   # 200
curl -I http://kvm.example.com:6081/vnc.html        # 301 vers https://…:6443
```

Avec un certificat auto-signé, ajoutez `--cacert tls/cert.pem` à curl.

Swagger est sur `https://kvm.example.com:8443/swagger-ui/`.

Les logs (une ligne par requête, avec le conteneur joint et le code de réponse) :

```bash
docker compose logs -f api-haproxy novnc-haproxy
```

## Problèmes courants

- **HAProxy ne démarre pas, `unable to load SSL private key`** : la clé est chiffrée par une
  phrase de passe, illisible par l'UID 99, ou ne correspond pas au certificat. Les deux
  commandes suivantes doivent afficher la même empreinte :
  `openssl x509 -in cert.pem -noout -pubkey | sha256sum` et
  `openssl pkey -in key.pem -pubout | sha256sum`.
- **`unable to stat SSL certificate` ou `Permission denied`** : `TLS_CERT_PATH` ou
  `TLS_KEY_PATH` pointe vers un fichier qui n'existe pas, ou que l'UID 99 ne peut pas lire.
- **`bind source path does not exist`** au démarrage : `TLS_CERT_PATH` ou `TLS_KEY_PATH`
  pointe vers un fichier absent. Vérifiez les chemins dans `.env` (un chemin relatif part du
  dossier du `docker-compose.yml`).
- **`no SSL certificate specified for bind ':8443'`** : HAProxy n'a trouvé aucun certificat à
  l'endroit monté. Vérifiez que `TLS_CERT_PATH` désigne bien un fichier, pas un dossier.
- **La console noVNC ne se connecte pas** : `NOVNC_URL` n'est pas l'adresse HTTPS du HAProxy
  de noVNC (`https://…:6443/vnc.html`), ou le certificat n'a pas encore été accepté sur ce
  port dans le navigateur.
- **`port is already allocated`** au démarrage : un autre service (souvent le HAProxy du
  frontend, sur 80 et 443) utilise déjà ce port. Changez `API_*_PORT` ou `NOVNC_*_PORT`.
- **`429 Too Many Requests` sur `/api/auth/…`** : la limite de 20 requêtes / 10 s est
  atteinte. Attendez quelques secondes.

## Ce que HAProxy ne couvre pas

Les HAProxy protègent le trajet entre le navigateur et le serveur. Deux points restent à traiter
à part :

- **Le port VNC des VM sur l'hyperviseur** : il est ouvert sur le réseau sans mot de passe.
  Limitez `VNC_LISTEN` et filtrez les ports 5900-5999 pour qu'ils ne soient joignables que
  depuis la machine qui fait tourner noVNC.
- **Le trajet noVNC → hyperviseur** : le protocole VNC n'y est pas chiffré. Sur un réseau
  qui n'est pas de confiance, activez VNC sur TLS dans libvirt (`vnc_tls`) ou passez par un
  tunnel.
