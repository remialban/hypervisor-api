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

Les routes VM ne stockent rien dans SQLite. `GET .../vms` et `GET .../vms/{vm_id}` lisent libvirt (`vm_id` = UUID libvirt, erreurs 400, 404 et 502). `POST .../start` et `POST .../stop` agissent sur libvirt et renvoient la VM (200) ou 409 si l'etat ne le permet pas. `stop` envoie un arret ACPI asynchrone (l'etat renvoye peut encore etre `running`) ; `?force=true` coupe immediatement l'alimentation. `POST .../clone` (corps `{"name": "..."}`) est asynchrone : il repond `202` avec un job (`Location: /api/jobs/{id}`). La VM source doit etre arretee ; ses disques sont copies dans leur pool de stockage et la copie est une VM arretee avec un nouvel UUID et une nouvelle MAC. `GET /api/jobs/{job_id}` donne le statut (`running`, `succeeded` avec `result`, `failed` avec `error`) ; les jobs sont en memoire et perdus au redemarrage. `DELETE .../vms/{vm_id}` supprime une VM arretee (204 ; 409 si elle tourne) en conservant ses fichiers disque ; `?delete_disks=true` supprime aussi les volumes, sauf s'ils sont partages avec une autre VM ou hors pool (409, rien n'est supprime). Seule la creation (`POST .../vms`) repond encore `501 Not Implemented`.

```text
GET    /api/hypervisors/{hypervisor_id}/vms
POST   /api/hypervisors/{hypervisor_id}/vms
GET    /api/hypervisors/{hypervisor_id}/vms/{vm_id}
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/start
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop
POST   /api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone
GET    /api/jobs/{job_id}
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

Les routes disques sont exposees, mais ne stockent rien dans SQLite et repondent actuellement `501 Not Implemented`.

```text
GET    /api/hypervisors/{hypervisor_id}/disks
POST   /api/hypervisors/{hypervisor_id}/disks
GET    /api/hypervisors/{hypervisor_id}/disks/{disk_id}
PATCH  /api/hypervisors/{hypervisor_id}/disks/{disk_id}
DELETE /api/hypervisors/{hypervisor_id}/disks/{disk_id}
```

Corps prevu :

```json
{
  "name": "vm-test-disk",
  "size_gb": 20,
  "format": "qcow2"
}
```

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
