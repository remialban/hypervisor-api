# HTTPS avec HAProxy

Avec Docker Compose, l'API et noVNC ne sont joignables que par HAProxy, en HTTPS. HAProxy
chiffre la connexion avec votre certificat, puis transmet chaque requête au bon conteneur :

| Chemin | Conteneur |
|---|---|
| `/api/…`, `/swagger-ui…`, `/api-docs/…`, `/ping` | `api` (port 3000) |
| tout le reste : `/vnc.html`, `/websockify`, fichiers de noVNC | `novnc` (port 6080) |

Les deux services partagent donc la même adresse, par exemple `https://kvm.example.com`.
Le port HTTP ne sert qu'à rediriger vers HTTPS.

La configuration est dans [haproxy/haproxy.cfg](../haproxy/haproxy.cfg).

## Ce que HAProxy apporte

- **Chiffrement** : TLS 1.2 et 1.3 uniquement, avec la liste de chiffrements « intermediate »
  de Mozilla. Les mots de passe, les jetons JWT, les jetons noVNC et tout ce qui passe par la
  console sont chiffrés entre le navigateur et le serveur.
- **Ports fermés** : noVNC n'est plus publié du tout. L'API n'est publiée que sur
  `127.0.0.1:3000`, donc seulement pour la machine elle-même (Bruno, curl). Pour la fermer
  aussi, supprimez la section `ports` du service `api` dans `docker-compose.yml`.
- **Limite sur l'authentification** : au plus 20 requêtes sur `/api/auth/…` toutes les 10
  secondes par adresse IP, puis `429`. Cette limite s'ajoute à celle de l'API, qui bloque un
  compte 15 minutes après 5 mots de passe faux.
- **En-têtes de sécurité** :
  - `Strict-Transport-Security` : le navigateur n'utilisera plus que HTTPS pour ce site ;
  - `Referrer-Policy: no-referrer` : l'URL noVNC, qui contient le jeton, n'est jamais
    envoyée à un autre site ;
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
docker compose restart haproxy
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
serveur. Le navigateur affichera un avertissement qu'il faut accepter une fois ; l'API et
noVNC ayant la même adresse, cela vaut pour les deux.

## Configuration `.env`

```dotenv
# Certificat (+ chaîne) et clé privée, en PEM, dans deux fichiers.
TLS_CERT_PATH=/etc/hypervisor-api/tls/cert.pem
TLS_KEY_PATH=/etc/hypervisor-api/tls/key.pem

# Ports publiés sur l'hôte (par défaut 80 et 443).
HTTP_PORT=80
HTTPS_PORT=443

# Adresse de noVNC vue par le navigateur : le nom du certificat, en https.
NOVNC_URL=https://kvm.example.com/vnc.html
```

`NOVNC_URL` doit être en `https://` et utiliser le nom du certificat. noVNC ouvre alors la
console en `wss://` (WebSocket chiffré) sur la même adresse. Avec un port HTTPS autre que
443, ajoutez-le : `https://kvm.example.com:8443/vnc.html`.

Si le front Angular est servi par la même adresse, laissez `CORS_ALLOWED_ORIGINS` vide.
S'il est ailleurs, mettez son origine en `https://`.

## Démarrage et vérification

```bash
docker compose up --build -d
curl https://kvm.example.com/ping          # {"status":"ok","content":"pong"}
curl -I http://kvm.example.com/vnc.html    # 301 vers https://
```

Avec un certificat auto-signé, ajoutez `--cacert tls/cert.pem` à curl.

Swagger est sur `https://kvm.example.com/swagger-ui/`.

Les logs de HAProxy (une ligne par requête, avec le conteneur choisi et le code de
réponse) :

```bash
docker compose logs -f haproxy
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
- **La console noVNC ne se connecte pas** : `NOVNC_URL` est encore en `http://` ou sur
  l'ancien port 6080. Elle doit être l'adresse HTTPS de HAProxy.
- **`429 Too Many Requests` sur `/api/auth/…`** : la limite de 20 requêtes / 10 s est
  atteinte. Attendez quelques secondes.

## Ce que HAProxy ne couvre pas

HAProxy protège le trajet entre le navigateur et le serveur. Deux points restent à traiter
à part :

- **Le port VNC des VM sur l'hyperviseur** : il est ouvert sur le réseau sans mot de passe.
  Limitez `VNC_LISTEN` et filtrez les ports 5900-5999 pour qu'ils ne soient joignables que
  depuis la machine qui fait tourner noVNC.
- **Le trajet noVNC → hyperviseur** : le protocole VNC n'y est pas chiffré. Sur un réseau
  qui n'est pas de confiance, activez VNC sur TLS dans libvirt (`vnc_tls`) ou passez par un
  tunnel.
