# Authentification, rôles et partage

Tout l'API (sauf `/ping` et `/api/auth/*`) exige un jeton :

```text
Authorization: Bearer <access_token>
```

Sans jeton valide : `401` avec l'en-tête `WWW-Authenticate: Bearer`.
Le détail de chaque route est dans Swagger (`/swagger-ui/`, bouton **Authorize**).

## Se connecter

Deux façons, qui renvoient la même réponse :

| Route | Corps | Pour qui |
|---|---|---|
| `POST /api/auth/login` | JSON `{"username","password"}` | le front, `curl` |
| `POST /api/auth/token` | formulaire OAuth2 `grant_type=password&username=…&password=…` | Swagger UI, clients OAuth2 génériques |

```json
{
  "access_token": "eyJ0eXAiOiJKV1Qi…",
  "token_type": "Bearer",
  "expires_in": 900,
  "refresh_token": "53114e2810fd…"
}
```

- **`access_token`** : JWT signé (HS256), valable 15 minutes. Il ne contient que l'identité (`sub`) :
  le rôle, les droits et l'état du compte sont relus en base à chaque requête. Un compte désactivé
  ou un changement de rôle est donc effectif immédiatement.
- **`refresh_token`** : jeton opaque, valable 7 jours, **à usage unique**. `POST /api/auth/refresh`
  (JSON `{"refresh_token"}`) ou `POST /api/auth/token` avec `grant_type=refresh_token` renvoie une
  nouvelle paire. Seul son SHA-256 est stocké en base.
- `POST /api/auth/logout` révoque le refresh token. L'access token reste valable jusqu'à son expiration.
- Après 5 échecs de connexion sur un nom d'utilisateur : `429` pendant 15 minutes.
- Une mauvaise combinaison nom/mot de passe donne toujours le même `401` (impossible de deviner
  quels noms existent). Le nom d'utilisateur ne tient pas compte de la casse.

Un mot de passe fait entre 8 et 128 caractères, un nom d'utilisateur entre 3 et 32
(lettres, chiffres, `.`, `_`, `-`).

## Comptes

| Qui | Quoi |
|---|---|
| Admin | `POST /api/users` crée un compte (`role` : `user` ou `admin`), `PATCH` change le rôle ou désactive, `DELETE` supprime, `PUT /api/users/{id}/password` réinitialise le mot de passe |
| Utilisateur connecté | `GET /api/me`, `PUT /api/me/password` (avec l'ancien mot de passe) |
| Tout le monde | `POST /api/auth/register`, **seulement si l'admin a ouvert les inscriptions** |

- Les inscriptions sont **fermées par défaut**. L'admin les ouvre ou les ferme avec
  `PUT /api/settings {"registration_open": true}`. Le front peut lire l'état sans être connecté avec
  `GET /api/auth/config`. Une inscription crée toujours un compte `user`.
- Changer ou réinitialiser un mot de passe ferme toutes les sessions du compte.
- Le système garde toujours un administrateur actif : on ne peut pas supprimer, désactiver ou
  rétrograder le dernier.
- **Premier démarrage** : si la base ne contient aucun compte, l'API crée un administrateur avec
  `ADMIN_USERNAME` (défaut `admin`) et `ADMIN_PASSWORD`. Sans `ADMIN_PASSWORD`, un mot de passe
  aléatoire est affiché une seule fois dans les logs.

## Rôles et droits

Inspiré de Proxmox : un rôle global, des droits attachés aux hyperviseurs et aux VM.

| Action | Admin | Utilisateur | Emprunteur d'une VM |
|---|---|---|---|
| Ajouter / modifier / supprimer un hyperviseur | ✅ | ❌ | ❌ |
| Voir l'URI d'un hyperviseur | ✅ | ❌ (champ vide) | ❌ |
| Donner / retirer l'accès à un hyperviseur | ✅ | ❌ | ❌ |
| Disques, pools, réseaux | ✅ | ❌ | ❌ |
| Voir un hyperviseur | tous | ceux où il a accès ou où il a une VM | celui de la VM prêtée |
| Voir les options de création (`vm-options`) | ✅ | avec accès à l'hyperviseur | ❌ |
| Créer une VM | partout | **seulement sur un hyperviseur où il a accès** | ❌ |
| Créer une VM pour quelqu'un d'autre (`owner_id`) | ✅ | ❌ | ❌ |
| Voir une VM | toutes | les siennes | celles prêtées |
| Console VNC, démarrer, arrêter | toutes | les siennes | celles prêtées |
| Cloner, supprimer | toutes | les siennes | ❌ |
| Prêter une VM, voir/retirer les prêts | toutes | les siennes | ❌ (il peut seulement rendre la VM) |
| Changer le propriétaire d'une VM | ✅ | ❌ | ❌ |
| Quotas, paramètres, journal d'audit, liste des comptes | ✅ | ❌ | ❌ |

Précisions :

- Quelqu'un qui n'a aucun droit sur une VM reçoit `404`, comme si elle n'existait pas.
  `403` signifie « elle existe pour vous, mais pas cette action ».
- Retirer l'accès d'un utilisateur à un hyperviseur ne lui enlève **pas** les VM qu'il possède déjà.
- Le propriétaire est stocké par l'API (libvirt n'en connaît pas). Une VM créée en dehors de l'API
  n'a pas de propriétaire : seuls les admins la voient, jusqu'à `PUT …/vms/{id}/owner`.
- Cloner : le clone appartient à celui qui le demande et compte dans son quota.
- Supprimer un utilisateur est refusé (`409`) tant qu'il possède des VM.

### Prêter une VM

`POST /api/hypervisors/{h}/vms/{vm}/shares` avec `{"username": "bob"}` (et éventuellement
`"expires_at"` en secondes Unix). Le propriétaire garde la VM ; l'emprunteur obtient la console et
démarrer/arrêter, rien d'autre, et ne peut pas la prêter plus loin. Un prêt prend fin :
à la suppression de la VM, à la révocation (`DELETE …/shares/{user_id}`, par le propriétaire, un admin
ou l'emprunteur lui-même), ou à `expires_at`.

### Quotas

Aucun par défaut. Un admin les fixe par utilisateur avec `PUT /api/users/{id}/quota`
(`max_vms`, `max_vcpus`, `max_memory_mb`, `max_disk_gb`, `null` = illimité). Dépassement : `403`
avec le nom de la limite. Une VM créée par un admin pour un utilisateur compte chez cet utilisateur.
`GET /api/me` donne le quota et l'usage courants.

## Console VNC

`GET /api/hypervisors/{h}/vms/{vm}/vnc` vérifie le droit d'accès, puis crée un **jeton aléatoire de
2 minutes** (`VNC_TOKEN_TTL_SECONDS`) qui associe `hôte:port` dans le fichier lu par websockify. Il
n'est plus l'UUID de la VM : ouvrir `novnc_url` fonctionne, deviner un UUID non. Le jeton disparaît du fichier au plus 10 secondes après son expiration. Il n'est
valable que pour établir la connexion ; la console ouverte continue ensuite. Rappeler la route pour
en obtenir un nouveau (par exemple pour se reconnecter).

## Journal d'audit

`GET /api/audit` (admin) : connexions réussies et échouées, création / modification / suppression de
comptes, de VM et d'hyperviseurs, prêts, accès console, changements de paramètres et de quotas.
Les mots de passe et jetons n'y figurent jamais. Filtres : `user_id`, `action`, `limit`, `offset`.

## Configuration (variables d'environnement)

| Variable | Rôle | Défaut |
|---|---|---|
| `JWT_SECRET` (ou `JWT_SECRET_FILE`) | Clé de signature, 32 caractères minimum | aléatoire à chaque démarrage (les sessions ne survivent pas à un redémarrage) |
| `ADMIN_USERNAME` / `ADMIN_PASSWORD` | Premier administrateur, si la base n'a aucun compte | `admin` / mot de passe généré et affiché |
| `ACCESS_TOKEN_TTL_SECONDS` | Durée de l'access token | `900` |
| `REFRESH_TOKEN_TTL_SECONDS` | Durée du refresh token | `604800` |
| `VNC_TOKEN_TTL_SECONDS` | Durée d'un jeton noVNC | `120` |
| `CORS_ALLOWED_ORIGINS` | Origines autorisées à appeler l'API depuis un navigateur, séparées par des virgules | aucune |
| `DATABASE_URL` | Base PostgreSQL (obligatoire) | aucun (`postgres://…@db:5432/hypervisor` dans Docker) |
| `VM_CONFIG_PATH` | Fichier de jetons de websockify | `vm.cfg` (`/vnc/vm.cfg` dans Docker) |

## Brancher le front Angular (`hypervisor-ui`)

En développement, `proxy.conf.json` envoie déjà `/api` vers `127.0.0.1:3000` : même origine, donc
pas de CORS à configurer. Sur un autre domaine ou un autre port, renseigner `CORS_ALLOWED_ORIGINS`
(par exemple `http://localhost:4200`).

Ce qu'il faut côté front, dans l'ordre :

1. Un service d'authentification : `login()` (`POST /api/auth/login`), garde `access_token` **en
   mémoire** (un signal), `refresh_token` dans `sessionStorage` si l'on veut survivre à un
   rechargement, `logout()`.
2. Un intercepteur HTTP qui ajoute `Authorization: Bearer …` aux appels `/api`, et qui, sur un `401`,
   appelle une fois `POST /api/auth/refresh` puis rejoue la requête ; si le refresh échoue, retour à
   la page de connexion. Le refresh token est à usage unique : ne lancer qu'un refresh à la fois et
   faire attendre les autres requêtes.
3. Un garde de route (`CanActivateFn`) qui renvoie vers `/login` sans session, et un second pour les
   pages réservées aux admins (`user().role === 'admin'`, lu via `GET /api/me`).
4. Les champs `owner` et `access` (`admin` / `owner` / `shared`) de chaque VM permettent d'afficher
   à qui elle appartient et de masquer « Supprimer » / « Cloner » pour un emprunteur.
5. `GET /api/auth/config` indique s'il faut montrer le lien « Créer un compte ».

Les erreurs gardent le format `{"status":"error","content":"…"}` : l'`error.interceptor.ts` existant
peut donc les afficher tel quel (`401` connexion, `403` droit manquant, `409` conflit, `429` trop
d'essais).
