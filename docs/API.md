# Documentation API

Base URL : `http://127.0.0.1:3000`

## Stockage

SQLite (`api.db`) stocke uniquement les hyperviseurs et leurs liens de connexion :

- `id`
- `name`
- `uri`
- `status`

Les VM, disques et réseaux ne sont pas stockes dans SQLite. Ces ressources seront gerees directement avec libvirt lorsque les handlers seront implementes.

## Sante

### `GET /ping`

Reponse `200` :

```json
{"status":"ok","content":"pong"}
```

## Hyperviseurs

### `GET /api/hypervisors`

Liste les hyperviseurs enregistres. Le statut est revalide aupres de libvirt.

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

Les routes VM ne stockent rien dans SQLite. `GET .../vms` et `GET .../vms/{vm_id}` lisent libvirt (`vm_id` = UUID libvirt, erreurs 400, 404 et 502). `POST .../start` et `POST .../stop` agissent sur libvirt et renvoient la VM (200) ou 409 si l'etat ne le permet pas. `stop` envoie un arret ACPI asynchrone (l'etat renvoye peut encore etre `running`) ; `?force=true` coupe immediatement l'alimentation. Les autres routes repondent encore `501 Not Implemented`.

```text
GET    /api/hypervisors/{hypervisor_id}/vms
POST   /api/hypervisors/{hypervisor_id}/vms
GET    /api/hypervisors/{hypervisor_id}/vms/{vm_id}
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/start
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone
DELETE /api/hypervisors/{hypervisor_id}/vms/{vm_id}
```

Corps prevu pour la creation :

```json
{
  "name": "vm-test",
  "memory_mb": 2048,
  "vcpus": 2
}
```

## Disques

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

Les routes reseaux sont exposees, mais ne stockent rien dans SQLite et repondent actuellement `501 Not Implemented`.

```text
GET  /api/hypervisors/{hypervisor_id}/networks
POST /api/hypervisors/{hypervisor_id}/networks
```

Corps prevu :

```json
{
  "name": "reseau-test",
  "bridge": "virbr10"
}
```

## VNC

### `GET /api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc`

Retourne actuellement `501 Not Implemented`. Aucun acces VNC n'est stocke dans SQLite.

## Documentation interactive

Swagger UI :

```text
http://127.0.0.1:3000/swagger-ui/
```

OpenAPI JSON :

```text
http://127.0.0.1:3000/api-docs/openapi.json
```
