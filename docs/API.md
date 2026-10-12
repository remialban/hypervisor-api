# Documentation API

Base URL : `http://127.0.0.1:3000`

Toutes les donnees sont stockees dans SQLite (`api.db`). Les identifiants sont des UUID.

## Reponse d'erreur

```json
{
  "statut": "erreur",
  "contenu": "Message d'erreur"
}
```

## Sante

### `GET /ping`

Verifie que l'API repond.

Reponse `200` :

```json
{"statut":"ok","contenu":"pong"}
```

## Hyperviseurs

### `GET /api/hypervisors`

Liste les hyperviseurs enregistres. Le statut est revalide aupres de libvirt.

### `POST /api/hypervisors`

Ajoute un hyperviseur apres verification de sa connexion libvirt.

Corps :

```json
{
  "nom": "kvm-local",
  "uri": "qemu:///system"
}
```

Reponses : `201` si cree, `502` si libvirt est inaccessible.

### `GET /api/hypervisors/{id}`

Retourne un hyperviseur et son statut courant.

### `PATCH /api/hypervisors/{id}`

Modifie le nom ou l'URI. Les champs sont optionnels :

```json
{"nom":"kvm-production"}
```

```json
{"uri":"qemu+ssh://admin@serveur/system"}
```

Reponses : `200`, `404`, `502`.

### `DELETE /api/hypervisors/{id}`

Supprime l'enregistrement de l'API et de SQLite. Cette route ne supprime pas l'hote KVM.

Reponses : `204` ou `404`.

## Machines virtuelles

`{hypervisor_id}` est l'identifiant de l'hyperviseur et `{vm_id}` celui de la VM.

### `GET /api/hypervisors/{hypervisor_id}/vms`

Liste les VM stockees pour un hyperviseur.

### `POST /api/hypervisors/{hypervisor_id}/vms`

Enregistre une VM :

```json
{
  "nom": "vm-test",
  "memoire_mb": 2048,
  "vcpus": 2
}
```

Reponse : `201` avec une VM au statut `arretee`.

### `GET /api/hypervisors/{hypervisor_id}/vms/{vm_id}`

Retourne une VM.

### `POST /api/hypervisors/{hypervisor_id}/vms/{vm_id}/start`

Met la VM au statut `demarree` dans SQLite.

### `POST /api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop`

Met la VM au statut `arretee` dans SQLite.

### `POST /api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone`

Cree une nouvelle entree VM avec le corps fourni :

```json
{
  "nom": "vm-clone",
  "memoire_mb": 2048,
  "vcpus": 2
}
```

### `DELETE /api/hypervisors/{hypervisor_id}/vms/{vm_id}`

Supprime la VM de SQLite.

## Disques

### `GET /api/hypervisors/{hypervisor_id}/disks`

Liste les disques persistants.

### `POST /api/hypervisors/{hypervisor_id}/disks`

Cree une entree disque :

```json
{
  "nom": "vm-test-disk",
  "taille_gb": 20,
  "format": "qcow2"
}
```

### `GET /api/hypervisors/{hypervisor_id}/disks/{disk_id}`

Retourne un disque.

### `PATCH /api/hypervisors/{hypervisor_id}/disks/{disk_id}`

Modifie le nom, la taille ou le format du disque avec le meme corps JSON que la creation.

### `DELETE /api/hypervisors/{hypervisor_id}/disks/{disk_id}`

Supprime l'entree disque de SQLite.

## Reseaux

### `GET /api/hypervisors/{hypervisor_id}/networks`

Liste les reseaux stockes.

### `POST /api/hypervisors/{hypervisor_id}/networks`

Cree une entree reseau :

```json
{
  "nom": "default",
  "bridge": "virbr0"
}
```

`bridge` est optionnel et peut etre `null`.

## VNC

### `GET /api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc`

Point d'extension pour retourner les informations de connexion VNC.

Cette route repond actuellement `501 Not Implemented`; elle ne demarre pas encore de proxy VNC.

## Exemples complets

Creer une VM :

```bash
curl -X POST http://127.0.0.1:3000/api/hypervisors/HYPERVISOR_ID/vms \
  -H 'Content-Type: application/json' \
  -d '{"nom":"vm-test","memoire_mb":2048,"vcpus":2}'
```

Lister les disques :

```bash
curl http://127.0.0.1:3000/api/hypervisors/HYPERVISOR_ID/disks
```

Creer un reseau :

```bash
curl -X POST http://127.0.0.1:3000/api/hypervisors/HYPERVISOR_ID/networks \
  -H 'Content-Type: application/json' \
  -d '{"nom":"reseau-test","bridge":"virbr10"}'
```
