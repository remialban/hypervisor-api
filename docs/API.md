# Référence de l'API

Toutes les routes de l'API, regroupées par thème. La référence générée depuis le code, avec
chaque champ et chaque code de réponse, est dans Swagger : `/swagger-ui/` (et la
spécification brute sur `/api-docs/openapi.json`).

Le fonctionnement d'ensemble est décrit dans [ARCHITECTURE.md](ARCHITECTURE.md), les droits
en détail dans [AUTH.md](AUTH.md).

## Conventions

### Adresse

| Contexte | Adresse de base |
|---|---|
| Développement (`cargo run`) | `http://127.0.0.1:3000` |
| Docker, depuis le réseau | `https://<serveur>:8443` (HAProxy de l'API) |
| Depuis l'application web | `https://<serveur>/api/…` (relayé par l'interface) |

Dans les exemples, les chemins sont relatifs à cette adresse.

### Authentification

Toutes les routes demandent `Authorization: Bearer <access_token>`, sauf `/ping`,
`/api/auth/*` et la documentation (`/swagger-ui`, `/api-docs`).

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:3000/api/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"…"}' | jq -r .access_token)

curl http://127.0.0.1:3000/api/hypervisors -H "Authorization: Bearer $TOKEN"
```

Le jeton d'accès dure 15 minutes. Le `refresh_token`, à usage unique, en donne un nouveau
(`POST /api/auth/refresh`).

### Codes de réponse et erreurs

| Code | Sens |
|---|---|
| `200`, `201`, `204` | Réussite (`204` : pas de corps). |
| `202` | Opération lancée en arrière-plan : suivre la **tâche** (voir ci-dessous) ou la ressource indiquée par l'en-tête `Location`. |
| `400` | Corps ou paramètre invalide. |
| `401` | Pas de jeton, ou jeton expiré. |
| `403` | Connecté, mais pas le droit (rôle, quota, limite d'un template). |
| `404` | La ressource n'existe pas, **ou l'appelant n'a aucun droit dessus** : on ne révèle pas son existence. |
| `409` | Conflit : nom déjà pris, VM allumée, build déjà en cours… |
| `429` | Trop d'essais de connexion. |
| `501` | Opération non prise en charge par le pilote libvirt. |
| `502` | L'hyperviseur (libvirt) n'a pas répondu ou a renvoyé une erreur. |

Les erreurs ont toujours la même forme :

```json
{"status": "error", "content": "Template not found"}
```

### Tâches en arrière-plan

Les opérations longues (cloner une VM, créer une VM depuis un template) répondent `202`
avec une tâche, et l'en-tête `Location: /api/jobs/{job_id}` :

```json
{"id": "6f1c…", "kind": "clone", "status": "running", "result": null, "error": null}
```

`GET /api/jobs/{job_id}` donne son état : `running`, puis `succeeded` (la VM créée dans
`result`) ou `failed` (le message dans `error`). Les tâches sont gardées en mémoire : elles
disparaissent au redémarrage de l'API. Chacun ne voit que les siennes, un admin les voit
toutes.

## Santé

| Route | Qui | Rôle |
|---|---|---|
| `GET /ping` | tout le monde | `{"status":"ok","content":"pong"}` : l'API répond. |

## Authentification et compte

| Route | Qui | Rôle |
|---|---|---|
| `POST /api/auth/login` | public | Connexion : `{"username","password"}` → `access_token`, `refresh_token`, `expires_in`, `token_type`. |
| `POST /api/auth/token` | public | La même chose au format OAuth2 (formulaire `grant_type=password` ou `refresh_token`), utilisé par Swagger. |
| `POST /api/auth/refresh` | public | `{"refresh_token"}` → une nouvelle paire de jetons. L'ancien refresh token ne marche plus. |
| `POST /api/auth/logout` | public | `{"refresh_token"}` : ferme la session. |
| `POST /api/auth/register` | public | Crée un compte `user` (`{"username","password"}`), seulement si l'inscription libre est ouverte (`403` sinon). |
| `GET /api/auth/config` | public | `{"registration_open": bool}`, pour afficher ou non « S'inscrire ». |
| `GET /api/me` | connecté | Mon compte : rôle, quota et usage actuel (VM, processeurs, mémoire, disque). |
| `PUT /api/me/password` | connecté | `{"current_password","new_password"}` (8 caractères minimum). |
| `GET /api/me/groups` | connecté | Les groupes dont je fais partie, directement ou par un sous-groupe (`direct`). Lecture seule. |

Un compte est bloqué 15 minutes après 5 mots de passe faux (`429`).

## Utilisateurs (admin)

| Route | Rôle |
|---|---|
| `GET /api/users` | Tous les comptes, avec quota et usage. |
| `POST /api/users` | Crée un compte : `{"username","password","role"}` (`role` : `user` par défaut, ou `admin`). `409` si le nom existe, sans tenir compte de la casse. |
| `GET /api/users/{user_id}` | Un compte. |
| `PATCH /api/users/{user_id}` | `{"role"}` et/ou `{"disabled": true}`. Le dernier admin actif ne peut être ni rétrogradé ni désactivé (`409`). |
| `DELETE /api/users/{user_id}` | Supprime le compte. `409` s'il possède encore des VM, ou si c'est le dernier admin. |
| `PUT /api/users/{user_id}/password` | Réinitialise le mot de passe : `{"new_password"}`. |
| `PUT /api/users/{user_id}/quota` | `{"max_vms","max_vcpus","max_memory_mb","max_disk_gb"}` ; `null` = illimité. |
| `GET /api/users/{user_id}/groups` | Les groupes d'un utilisateur. |
| `GET /api/settings`, `PUT /api/settings` | `{"registration_open": bool}` : ouvre ou ferme l'inscription libre. |
| `GET /api/audit` | Journal : qui a fait quoi. Filtres `?user_id=`, `?action=`, pagination `?limit=&offset=`. |

## Groupes (admin)

Un groupe contient des utilisateurs et d'autres groupes (sous-groupes, sans limite de
profondeur). Un utilisateur peut être dans plusieurs groupes. Les groupes servent à donner
des droits sur les templates. Un utilisateur voit seulement les siens (`GET /api/me/groups`).

| Route | Rôle |
|---|---|
| `GET /api/groups` | Tous les groupes, avec leur nombre de membres et de sous-groupes directs. |
| `POST /api/groups` | `{"name","description"}`. Nom unique, sans tenir compte de la casse (`409`). |
| `GET /api/groups/{group_id}` | Le groupe, ses membres directs, ses sous-groupes et les groupes qui le contiennent. |
| `PATCH /api/groups/{group_id}` | `{"name"}` et/ou `{"description"}`. |
| `DELETE /api/groups/{group_id}` | Supprime le groupe ; ses membres et sous-groupes sont gardés. |
| `GET /api/groups/{group_id}/members` | Tous les membres, sous-groupes compris (`direct: false` pour ceux qui n'y sont que par un sous-groupe). |
| `PUT` / `DELETE /api/groups/{group_id}/members/{user_id}` | Ajoute / retire un utilisateur. |
| `PUT` / `DELETE /api/groups/{group_id}/subgroups/{child_id}` | Met un groupe dans un autre / l'en retire. `409` si cela mettait un groupe dans lui-même. |

## Hyperviseurs

| Route | Qui | Rôle |
|---|---|---|
| `GET /api/hypervisors` | connecté | Les hyperviseurs visibles : tous pour un admin ; pour un utilisateur, ceux auxquels il a accès ou qui hébergent une de ses VM (sans `uri`). Le statut est revérifié auprès de libvirt. |
| `POST /api/hypervisors` | admin | `{"name","uri"}`, par exemple `qemu:///system` ou `qemu+ssh://root@192.168.1.10/system`. La connexion est testée (`502` si elle échoue). |
| `GET /api/hypervisors/{id}` | connecté | Un hyperviseur. |
| `PATCH /api/hypervisors/{id}` | admin | `{"name"}` et/ou `{"uri"}` (connexion retestée). |
| `DELETE /api/hypervisors/{id}` | admin | Retire l'hyperviseur de l'API. Rien n'est supprimé sur la machine. |
| `GET /api/hypervisors/{id}/access` | admin | Les utilisateurs qui peuvent créer des VM dessus. |
| `PUT` / `DELETE /api/hypervisors/{id}/access/{user_id}` | admin | Donne / retire ce droit. Les VM déjà créées restent à leur propriétaire. |

## Machines virtuelles

Préfixe : `/api/hypervisors/{hypervisor_id}`. `vm_id` est l'UUID libvirt de la VM.

Un utilisateur ne voit que ses VM et celles qu'on lui prête ; un admin voit tout. Chaque VM
porte `owner` (son propriétaire, `null` si elle a été créée hors de l'API) et `access`
(`admin`, `owner` ou `shared`).

| Route | Qui | Rôle |
|---|---|---|
| `GET …/vms` | connecté | Les VM visibles, avec état, mémoire et processeurs. |
| `GET …/vm-options` | accès à l'hyperviseur | Ce qu'on peut choisir pour créer une VM : pools et leurs volumes (disques, ISO, `used_by`), réseaux, firmwares, limites, valeurs par défaut. |
| `POST …/vms` | accès à l'hyperviseur | Crée une VM (voir ci-dessous). `201`. Un admin peut ajouter `"owner_id"` pour la créer au nom de quelqu'un. `403` si le quota est dépassé. |
| `GET …/vms/{vm_id}` | propriétaire, emprunteur, admin | Une VM. |
| `POST …/vms/{vm_id}/start` | propriétaire, emprunteur, admin | Démarre la VM. `409` si elle tourne déjà. |
| `POST …/vms/{vm_id}/stop` | propriétaire, emprunteur, admin | Arrêt propre (ACPI) ; `?force=true` coupe tout de suite. |
| `POST …/vms/{vm_id}/clone` | propriétaire, admin | `{"name"}` : copie une VM **arrêtée** (disques copiés, nouvel UUID, nouvelle MAC). `202` + tâche. |
| `DELETE …/vms/{vm_id}` | propriétaire, admin | Supprime une VM arrêtée (`409` sinon) en gardant ses disques ; `?delete_disks=true` les supprime aussi (sauf s'ils servent à une autre VM). |
| `GET …/vms/{vm_id}/vnc` | propriétaire, emprunteur, admin | Ouvre la console (voir [Console](#console-vnc)). |

Corps de `POST …/vms` :

```json
{
  "name": "vm-test",
  "memory_mb": 2048,
  "vcpus": 2,
  "disks": [
    { "pool": "default", "size_gb": 20 },
    { "pool": "default", "volume": "data.qcow2" }
  ],
  "iso": { "pool": "isos", "volume": "debian-13.iso" },
  "network": "default",
  "firmware": "bios",
  "start": false
}
```

- Obligatoires : `name`, `memory_mb`, `vcpus`, et au moins un disque ou une ISO.
- Par défaut : `network` = `default`, `firmware` = `bios` (ou `uefi`), `start` = `false`.
- Un disque est soit **neuf** (`size_gb` : un volume qcow2 créé pour l'occasion), soit
  **existant** (`volume`, qui ne doit servir à aucune autre VM).
- Pools, volumes et réseaux sont désignés par leur nom, jamais par un chemin.
- Tout est vérifié avant de créer quoi que ce soit ; en cas d'échec, les disques créés sont
  supprimés.

### Prêt et propriétaire

| Route | Qui | Rôle |
|---|---|---|
| `GET …/vms/{vm_id}/shares` | propriétaire, admin | Les personnes à qui la VM est prêtée. |
| `POST …/vms/{vm_id}/shares` | propriétaire, admin | `{"username","expires_at"}` (`expires_at` facultatif, en secondes Unix) : prête la VM (console, démarrage, arrêt). |
| `DELETE …/vms/{vm_id}/shares/{user_id}` | propriétaire, admin, l'emprunteur | Retire le prêt. |
| `PUT …/vms/{vm_id}/owner` | admin | `{"owner_id"}` : donne la VM à un autre utilisateur (y compris une VM créée hors de l'API). |

## Stockage et réseaux (admin)

Préfixe : `/api/hypervisors/{hypervisor_id}`. Tout est lu et modifié directement dans
libvirt.

| Route | Rôle |
|---|---|
| `GET …/disks` | Les volumes de tous les pools. |
| `POST …/disks` | `{"name","size_gb","format"}` (par exemple `qcow2`). |
| `GET …/disks/{disk_id}` | Un volume, par sa clé libvirt. |
| `PATCH …/disks/{disk_id}` | `{"size_gb"}` : agrandit le volume. Réduire est refusé (`400`). |
| `DELETE …/disks/{disk_id}` | Supprime le volume **et ses données**. |
| `GET …/pools` | Les pools de stockage. |
| `POST …/pools` | `{"name","target_path"}` : crée, construit et démarre un pool de type répertoire. |
| `GET …/networks` | Les réseaux virtuels, actifs ou non. |
| `POST …/networks` | `{"name","bridge"}` (`bridge` facultatif) : crée et démarre un réseau isolé. |

## Templates

Préfixe : `/api/hypervisors/{hypervisor_id}/templates`. Un template est une image de VM
construite par **Packer** sur l'hyperviseur. Chaque construction donne une **version**
numérotée ; on crée les VM depuis la dernière version construite.

Qui peut quoi :

- **admin** : tout, y compris créer et supprimer un template, changer sa visibilité et
  donner les droits ;
- droit **`manage`** : modifier, reconstruire, arrêter un build, lire les logs, voir tous
  les builds avec leur configuration, supprimer une version ;
- droit **`use`** : voir le template et ses versions construites, créer des VM.

Un template `hypervisor` (par défaut) est aussi utilisable par tous ceux qui ont accès à
l'hyperviseur ; un template `restricted`, seulement grâce aux droits. Chaque template
renvoie `access` : `level` (`use`, `manage`, `admin`), `max_vms` et `vm_count`.

| Route | Qui | Rôle |
|---|---|---|
| `GET …/templates` | connecté | Les templates sur lesquels j'ai un droit. |
| `POST …/templates` | admin | `{"name","packer_config","disk_name","pool","visibility","allow_vm_creation"}`. Lance le build de la version 1 : `202`. |
| `GET …/templates/{template_id}` | droit | Le template, sa dernière version, mon niveau d'accès. |
| `PATCH …/templates/{template_id}` | `manage` (visibilité : admin) | Modifie la définition ; s'applique au prochain build. |
| `DELETE …/templates/{template_id}` | admin | Supprime le template, ses versions et leurs disques (`409` pendant un build). Les VM déjà créées gardent leur copie. |
| `GET …/{template_id}/versions` | droit | Les versions (seulement celles construites pour un droit `use`). |
| `POST …/{template_id}/versions` | `manage` | Reconstruit : nouvelle version, `202`. `409` si un build tourne déjà. |
| `GET …/{template_id}/versions/{version}` | droit | Une version. |
| `DELETE …/{template_id}/versions/{version}` | `manage` | Supprime la version et son disque. |
| `GET …/{template_id}/versions/{version}/logs` | `manage` | La sortie du build, en direct : rappeler avec `?offset=<next_offset>` pour n'avoir que la suite. |
| `POST …/{template_id}/stop` | `manage` | Arrête le build en cours (Packer nettoie) ; `?force=true` le tue tout de suite. |
| `POST …/{template_id}/vms` | droit | Crée une VM : `{"name","memory_mb","vcpus","version","network","firmware","start"}`. `202` + tâche. `403` si le quota ou `max_vms` est atteint. |
| `GET …/{template_id}/permissions` | admin | Les droits donnés (utilisateurs, puis groupes), avec le nombre de VM de chacun. |
| `PUT` / `DELETE …/{template_id}/permissions/users/{user_id}` | admin | Donne (ou change) / retire le droit d'un utilisateur : `{"role": "use" \| "manage", "max_vms": 2}`. |
| `PUT` / `DELETE …/{template_id}/permissions/groups/{group_id}` | admin | Pareil pour un groupe, et donc pour tous ses membres, sous-groupes compris. |

`max_vms` est le nombre de VM issues du template que **chaque** personne peut posséder
(`null` = illimité). Quand plusieurs droits s'appliquent (le sien, ceux de ses groupes), le
plus généreux l'emporte. Les admins ne sont pas limités.

## Labos

Préfixe : `/api/hypervisors/{hypervisor_id}/labs`. Un labo est un ensemble de VM issues de
templates, branchées sur plusieurs réseaux, que l'on **déploie** en **instances**.

- **Réseaux** : `external` = un réseau libvirt **existant** de l'hyperviseur (NAT ou bridge vers
  Internet, par exemple `default`), partagé par toutes les instances ; `internal` = un réseau
  **isolé**, créé pour chaque instance (`lab-<instance>-<nom>`).
- **VM** : chacune vient d'un template (sa dernière version construite, ou une version
  donnée) et a des cartes réseau **dans l'ordre** : la première est la première vue par le
  système invité. Une VM `router` (OPNsense…) relie les réseaux : elle est créée et démarrée
  avant les autres. L'API ne fait que le câblage ; la configuration d'OPNsense vient de son
  template, ou se fait dans sa console.
- **Instance** : déployer un labo crée ses réseaux internes puis ses VM. Les VM
  appartiennent à celui qui déploie, comptent dans son quota, et les routes des VM marchent
  dessus (console, démarrage, arrêt). Une instance se supprime d'un bloc : VM, disques et
  réseaux internes.

Droits, comme pour les templates : **admin** (tout, dont créer et supprimer un labo, sa
visibilité et les droits) ; **`manage`** (modifier le labo, voir et piloter toutes ses
instances) ; **`use`** (voir le labo, déployer ses instances, piloter les siennes), avec
`max_instances` par personne. Un labo est `restricted` par défaut (seulement par les
droits) ; `hypervisor` l'ouvre aussi à tous ceux qui ont accès à l'hyperviseur. Un
gestionnaire ne peut mettre dans un labo que les templates qu'il a le droit d'utiliser.

**Définition du labo**

| Route | Qui | Rôle |
|---|---|---|
| `GET …/labs` | connecté | Les labos sur lesquels j'ai un droit, avec `access` (`level`, `max_instances`, `instance_count`). |
| `POST …/labs` | admin | `{"name","description","visibility"}`. |
| `GET …/labs/{lab_id}` | droit | Le labo, ses réseaux et ses VM (routeurs d'abord, cartes dans l'ordre). |
| `PATCH …/labs/{lab_id}` | `manage` (visibilité : admin) | `{"name","description","visibility"}`. |
| `DELETE …/labs/{lab_id}` | admin | `409` tant qu'il a des instances. |
| `POST …/labs/{lab_id}/networks` | `manage` | `{"name","kind":"external","libvirt_network":"default"}` ou `{"name","kind":"internal"}`. |
| `DELETE …/labs/{lab_id}/networks/{network_id}` | `manage` | `409` si une VM du labo ou une instance l'utilise. |
| `POST …/labs/{lab_id}/vms` | `manage` | Ajoute une VM (voir ci-dessous). |
| `PUT …/labs/{lab_id}/vms/{lab_vm_id}` | `manage` | Remplace la VM. Les instances déjà déployées gardent ce qu'elles ont. |
| `DELETE …/labs/{lab_id}/vms/{lab_vm_id}` | `manage` | `409` si une instance l'a encore. |
| `GET …/labs/{lab_id}/permissions` | admin | Les droits donnés, avec le nombre d'instances de chaque utilisateur. |
| `PUT` / `DELETE …/permissions/users/{user_id}` | admin | `{"role": "use" \| "manage", "max_instances": 1}`. |
| `PUT` / `DELETE …/permissions/groups/{group_id}` | admin | Pareil pour un groupe et tous ses membres, sous-groupes compris. |

Corps d'une VM de labo (un OPNsense avec un WAN et un LAN) :

```json
{
  "name": "opnsense",
  "role": "router",
  "template_id": "…",
  "version": null,
  "memory_mb": 2048,
  "vcpus": 2,
  "firmware": "bios",
  "networks": ["<id du réseau wan>", "<id du réseau lan1>"]
}
```

**Instances**

| Route | Qui | Rôle |
|---|---|---|
| `GET …/labs/{lab_id}/instances` | droit | Mes instances ; toutes pour `manage` et admin. |
| `POST …/labs/{lab_id}/instances` | droit | `{"name","start"}` : déploie. `202`, puis suivre l'instance jusqu'à `ready` ou `failed`. `403` si le quota ou `max_instances` est atteint ; `409` si un template n'est pas construit ou un réseau externe manque. |
| `GET …/instances/{instance_id}` | propriétaire, `manage`, admin | État (`deploying`, `ready`, `failed` + `error`), réseaux libvirt, et chaque VM : `vm_id`, état en direct, réseaux de ses cartes dans l'ordre. |
| `POST …/instances/{instance_id}/start` | propriétaire, `manage`, admin | Démarre toutes les VM, routeurs d'abord. |
| `POST …/instances/{instance_id}/stop` | propriétaire, `manage`, admin | Arrêt propre de toutes les VM (routeurs en dernier) ; `?force=true` les coupe. |
| `DELETE …/instances/{instance_id}` | propriétaire, `manage`, admin | Supprime les VM, leurs disques et les réseaux internes. `409` pendant le déploiement. |
| `GET /api/me/lab-instances` | connecté | Toutes mes instances, sur tous les hyperviseurs. |

Pour agir sur **une** VM d'une instance (console, démarrer, arrêter), utiliser les routes des
VM avec son `vm_id`. Si le déploiement échoue, ce qu'il avait créé est supprimé et
l'instance reste en `failed` avec la raison ; on la supprime ensuite comme les autres.

## Console (VNC)

`GET /api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc` (propriétaire, emprunteur ou admin)
crée un jeton aléatoire valable 2 minutes et renvoie l'adresse de la console :

```json
{
  "hypervisor_id": "…", "vm_id": "…", "uuid": "…",
  "host": "192.168.1.10", "port": "5900",
  "token": "9f2c…",
  "novnc_url": "https://<serveur>:6443/vnc.html?path=websockify/?token=9f2c…&autoconnect=true",
  "status": "connected", "expires_at": 1792706543
}
```

Il suffit d'ouvrir `novnc_url` dans le navigateur. `409` si la VM est arrêtée (pas encore
de port VNC).

## Tâches

| Route | Qui | Rôle |
|---|---|---|
| `GET /api/jobs/{job_id}` | celui qui l'a lancée, admin | État d'une tâche (voir [Tâches en arrière-plan](#tâches-en-arrière-plan)). |

## Pour essayer

- **Swagger** : `/swagger-ui/`, bouton **Authorize** pour se connecter.
- **Bruno** : une requête prête pour chaque route dans `docs/bruno/` (voir [BRUNO.md](BRUNO.md)).
