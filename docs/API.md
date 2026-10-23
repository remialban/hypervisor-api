# Documentation API

Base URL : `http://127.0.0.1:3000`

## Authentification

Toutes les routes demandent `Authorization: Bearer <access_token>`, sauf `/ping`, `/api/auth/*` et la documentation. Sans jeton valide : `401`. Rôle ou droit insuffisant : `403`. Ressource sur laquelle l'appelant n'a aucun droit : `404`. Le fonctionnement complet (rôles, matrice des droits, prêt, quotas, audit, front Angular) est dans [AUTH.md](AUTH.md).

| Route | Qui | Rôle |
|---|---|---|
| `POST /api/auth/login` | public | Connexion JSON `{"username","password"}` → `access_token`, `refresh_token`, `expires_in` |
| `POST /api/auth/token` | public | Même chose en formulaire OAuth2 (`grant_type=password` ou `refresh_token`) |
| `POST /api/auth/refresh` | public | `{"refresh_token"}` → nouvelle paire (le refresh token est à usage unique) |
| `POST /api/auth/logout` | public | `{"refresh_token"}` : révoque la session |
| `POST /api/auth/register` | public | Crée un compte `user`, seulement si les inscriptions sont ouvertes (`403` sinon) |
| `GET /api/auth/config` | public | `{"registration_open": bool}` |
| `GET /api/me` | connecté | Compte, quota et usage |
| `PUT /api/me/password` | connecté | `{"current_password","new_password"}` |
| `GET/POST /api/users` | admin | Liste / création (`{"username","password","role"}`) |
| `GET/PATCH/DELETE /api/users/{user_id}` | admin | Lecture / `{"role","disabled"}` / suppression (`409` s'il possède des VM) |
| `PUT /api/users/{user_id}/password` | admin | Réinitialise le mot de passe `{"new_password"}` |
| `PUT /api/users/{user_id}/quota` | admin | `{"max_vms","max_vcpus","max_memory_mb","max_disk_gb"}`, `null` = illimité |
| `GET/PUT /api/settings` | admin | `{"registration_open": bool}` |
| `GET /api/audit` | admin | Journal, filtres `user_id`, `action`, `limit`, `offset` |

Les erreurs ont toujours la forme `{"status":"error","content":"..."}`.

## Stockage

SQLite (`api.db`, schéma dans `migrations/`) stocke les hyperviseurs, les comptes, les sessions, les droits d'accès aux hyperviseurs, le propriétaire de chaque VM (par UUID libvirt), les prêts de VM, les jetons noVNC et le journal d'audit.

Les VM, disques et réseaux eux-mêmes ne sont pas stockes dans SQLite : libvirt reste leur source de vérité.

## Sante

### `GET /ping`

Reponse `200` :

```json
{"status":"ok","content":"pong"}
```

## Hyperviseurs

Ajouter, modifier et supprimer un hyperviseur est réservé aux admins. Les autres utilisateurs ne voient que les hyperviseurs auxquels un admin leur a donné accès ou qui hébergent l'une de leurs VM (le champ `uri` leur est renvoyé vide).

### `GET /api/hypervisors`

Liste les hyperviseurs visibles par l'appelant. Le statut est revalide aupres de libvirt.

### `GET /api/hypervisors/{id}/access`, `PUT|DELETE /api/hypervisors/{id}/access/{user_id}`

Admin. Liste, donne ou retire le droit de **créer des VM** sur cet hyperviseur. Retirer ce droit n'enlève pas les VM déjà possédées (`204`, `404`).

### `POST /api/hypervisors`

Ajoute un hyperviseur dans SQLite apres verification de sa connexion libvirt.

```json
{
  "name": "kvm-local",
  "uri": "qemu:///system"
}
```

Reponses : `201` si cree, `502` si libvirt est inaccessible.

### `GET /api/hypervisors/{id}`

Retourne un hyperviseur et son statut courant.

### `PATCH /api/hypervisors/{id}`

Modifie le nom ou l'URI, puis met a jour SQLite.

```json
{"name":"kvm-production"}
```

```json
{"uri":"qemu+ssh://admin@serveur/system"}
```

Reponses : `200`, `404`, `502`.

### `DELETE /api/hypervisors/{id}`

Supprime uniquement l'enregistrement de SQLite. Cette route ne supprime pas l'hote KVM.

Reponses : `204` ou `404`.

## VM

Un utilisateur ne voit que ses VM et celles qu'on lui prête ; un admin voit tout. Chaque VM renvoyée porte `owner` (nom du propriétaire, `null` si elle a été créée hors de l'API) et `access` (`admin`, `owner` ou `shared`). Créer une VM exige l'accès à l'hyperviseur ; un admin peut ajouter `"owner_id"` au corps pour la créer au nom d'un utilisateur. Cloner et supprimer sont réservés au propriétaire et aux admins ; démarrer, arrêter et la console sont ouverts aux emprunteurs. Un dépassement de quota donne `403`.

### Prêt et propriétaire

- `GET|POST /api/hypervisors/{h}/vms/{vm}/shares` : liste / prête la VM (`{"username","expires_at"?}`), propriétaire ou admin.
- `DELETE /api/hypervisors/{h}/vms/{vm}/shares/{user_id}` : retire un prêt (propriétaire, admin, ou l'emprunteur lui-même).
- `PUT /api/hypervisors/{h}/vms/{vm}/owner` : `{"owner_id"}`, admin. Donne une VM à un utilisateur, y compris une VM créée hors de l'API.

Les routes VM ne stockent rien dans SQLite. `GET .../vms` et `GET .../vms/{vm_id}` lisent libvirt (`vm_id` = UUID libvirt, erreurs 400, 404 et 502). `POST .../start` et `POST .../stop` agissent sur libvirt et renvoient la VM (200) ou 409 si l'etat ne le permet pas. `stop` envoie un arret ACPI asynchrone (l'etat renvoye peut encore etre `running`) ; `?force=true` coupe immediatement l'alimentation. `POST .../clone` (corps `{"name": "..."}`) est asynchrone : il repond `202` avec un job (`Location: /api/jobs/{id}`). La VM source doit etre arretee ; ses disques sont copies dans leur pool de stockage et la copie est une VM arretee avec un nouvel UUID et une nouvelle MAC. `GET /api/jobs/{job_id}` donne le statut (`running`, `succeeded` avec `result`, `failed` avec `error`) ; les jobs sont en memoire et perdus au redemarrage. `DELETE .../vms/{vm_id}` supprime une VM arretee (204 ; 409 si elle tourne) en conservant ses fichiers disque ; `?delete_disks=true` supprime aussi les volumes, sauf s'ils sont partages avec une autre VM ou hors pool (409, rien n'est supprime). `GET .../vm-options` donne ce qu'une interface peut proposer pour creer une VM : pools de stockage actifs avec leurs volumes (`kind` = `disk` ou `iso`, `used_by` = VM qui utilisent deja le disque), reseaux actifs, firmwares, bornes (`limits`) et valeurs par defaut (`defaults`), lus en direct dans libvirt. Seuls les fichiers reconnus comme image de disque (qcow2, vmdk..., ou `.img`/`.raw`) ou comme ISO sont listes : archives, scripts et autres fichiers d'un pool sont ignores. `POST .../vms` cree une VM (201, arretee sauf `"start": true`) a partir de ce que propose `vm-options` : voir ci-dessous.

```text
GET    /api/hypervisors/{hypervisor_id}/vms
POST   /api/hypervisors/{hypervisor_id}/vms
GET    /api/hypervisors/{hypervisor_id}/vms/{vm_id}
GET    /api/hypervisors/{hypervisor_id}/vm-options
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/start
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone
GET    /api/jobs/{job_id}
DELETE /api/hypervisors/{hypervisor_id}/vms/{vm_id}
```

Corps de la creation (`POST /api/hypervisors/{hypervisor_id}/vms`) :

```json
{
  "name": "vm-test",
  "memory_mb": 2048,
  "vcpus": 2,
  "disks": [
    { "pool": "datastore", "size_gb": 20 },
    { "pool": "datastore", "volume": "data.qcow2" }
  ],
  "iso": { "pool": "Téléchargements", "volume": "debian.iso" },
  "network": "default",
  "firmware": "bios",
  "start": false
}
```

- Obligatoires : `name`, `memory_mb`, `vcpus`, et au moins un disque ou une ISO. Defauts : `network` = `default`, `firmware` = `bios` (ou `uefi`), `start` = `false`.
- Chaque disque est soit **neuf** (`size_gb` : volume qcow2 fin `<nom>.qcow2`, puis `<nom>-disk1.qcow2`...), soit **existant** (`volume`, qui ne doit etre utilise par aucune autre VM).
- Les pools, volumes et reseaux sont designes par leur nom, jamais par un chemin : l'API ne peut pas rattacher un fichier arbitraire de l'hote.
- Tout est verifie avant de creer quoi que ce soit ; si la definition echoue, les disques crees sont supprimes. Les disques existants ne sont jamais supprimes.
- La VM utilise q35, virtio (disque, reseau, video), un affichage **VNC** (necessaire a la route `/vnc`) qui ecoute sur l'adresse de la variable d'environnement `VNC_LISTEN` (defaut `0.0.0.0`, car le proxy noVNC tourne sur la machine de l'API : a restreindre par pare-feu ou en donnant l'IP du reseau de l'hyperviseur), et une ISO bootee en premier si elle est fournie.
- Erreurs : 400 (valeurs invalides, volume qui n'est pas du bon type), 404 (hyperviseur, pool, volume ou reseau inconnu), 409 (nom de VM ou de volume deja pris, disque deja utilise, pool ou reseau inactif), 502.
- Limites : la creation est synchrone ; un disque est toujours cree en qcow2.

## Disques

Les routes disques, pools et réseaux sont réservées aux admins (`403` sinon).

Les disques et les pools sont geres directement par libvirt, sans stockage dans SQLite.

```text
GET    /api/hypervisors/{hypervisor_id}/disks
POST   /api/hypervisors/{hypervisor_id}/disks
GET    /api/hypervisors/{hypervisor_id}/disks/{disk_id}
PATCH  /api/hypervisors/{hypervisor_id}/disks/{disk_id}
DELETE /api/hypervisors/{hypervisor_id}/disks/{disk_id}
```

`GET .../disks/{disk_id}` lit un volume libvirt a partir de sa cle (`disk_id`) et renvoie un `Disk` (200), 404 si le volume est absent, ou 502 en cas d'erreur libvirt.

`PATCH .../disks/{disk_id}` accepte `{"size_gb":40}` et redimensionne le volume (200 avec le `Disk` mis a jour). La reduction de capacite est refusee (400) pour eviter une perte de donnees ; le nom et le format ne sont pas modifiables par cette route. Un pilote qui ne prend pas en charge cette operation renvoie `501`, notamment le pilote de test `test:///default`.

`DELETE .../disks/{disk_id}` supprime le volume libvirt identifie par sa cle et renvoie `204` sans corps. La route renvoie `404` si le volume est absent, `501` si le pilote ne prend pas en charge la suppression, ou `502` en cas d'autre erreur libvirt. Cette operation supprime les donnees du volume.

Corps prevu :

```json
{
  "name": "vm-test-disk",
  "size_gb": 20,
  "format": "qcow2"
}
```

### Pools de stockage

Les pools sont rattaches a un hyperviseur et listes/crees via libvirt. La creation definit un pool de type `dir`, construit le repertoire cible et demarre le pool.

```text
GET  /api/hypervisors/{hypervisor_id}/pools
POST /api/hypervisors/{hypervisor_id}/pools
```

Corps de creation :

```json
{
  "name": "default",
  "target_path": "/tmp/libvirt-pools/default"
}
```

La reponse de pool contient son UUID `id`, son `hypervisor_id`, son `name` et son etat `active`.

## Reseaux

Les réseaux sont lus et créés directement dans libvirt, sans stockage SQLite. La liste renvoie les réseaux actifs ou inactifs de l’hyperviseur. La création définit puis démarre un réseau isolé ; le champ `bridge` est facultatif.

```text
GET  /api/hypervisors/{hypervisor_id}/networks
POST /api/hypervisors/{hypervisor_id}/networks
```

Corps prevu :

```json
{
  "name": "reseau-test"
}
```

La création peut aussi préciser un nom de bridge, par exemple `"bridge":"virbr10"`. La réponse contient l’UUID `id`, le `hypervisor_id`, le nom, le bridge éventuel et le statut (`active` ou `inactive`).

## VNC

### `GET /api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc`

Propriétaire, emprunteur ou admin. Crée un jeton noVNC aléatoire qui expire (2 minutes par défaut), l'écrit dans le fichier lu par websockify et renvoie :

```json
{
  "hypervisor_id": "...", "vm_id": "...", "uuid": "...",
  "host": "kvm1.example", "port": "5900",
  "token": "9f2c...", "novnc_url": "http://localhost:6080/vnc.html?path=websockify/?token=9f2c...&autoconnect=true",
  "status": "connected", "expires_at": 1792706543
}
```

Reponses : `200`, `404` (VM inconnue ou sans droit, ou pas de port VNC), `409` (port pas encore attribué : la VM est arrêtée).

## Documentation interactive

Swagger UI :

```text
http://127.0.0.1:3000/swagger-ui/
```

OpenAPI JSON :

```text
http://127.0.0.1:3000/api-docs/openapi.json
```
