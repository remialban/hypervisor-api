# Hypervisor API

Une plateforme web pour gérer des machines virtuelles **KVM** réparties sur plusieurs
hyperviseurs. Depuis un navigateur, on crée, démarre et arrête des VM, on ouvre leur
console, on les construit à partir de templates, et on contrôle qui a le droit de faire
quoi : comptes, groupes, quotas, prêts de VM.

Le projet est découpé en deux dépôts :

| Dépôt | Rôle | Technologies |
|---|---|---|
| **`hypervisor-api`** (celui-ci) | Le **serveur**. Il pilote les hyperviseurs par libvirt (en local ou par SSH), garde les comptes, les droits et l'historique dans une base PostgreSQL, et expose une API REST. Il fournit aussi les consoles des VM par noVNC. | Rust, Axum, libvirt, PostgreSQL, Packer, noVNC |
| **`hypervisor-ui`** | L'**interface web**. Une application Angular qui affiche les écrans et appelle l'API. Elle ne parle jamais aux hyperviseurs et ne garde aucune donnée. | Angular, TypeScript, nginx |

Les deux tournent dans Docker, chacun derrière son HAProxy en HTTPS.

- **Installer en quelques commandes** : [DEPLOY.md](DEPLOY.md).
- **Installation détaillée** (développement ou Docker) : [INSTALLATION.md](INSTALLATION.md).
- **Architecture Docker** (conteneurs, ports, réseaux, schémas) : [docs/DOCKER.md](docs/DOCKER.md).
- **Référence de l'API** : Swagger sur `/swagger-ui/`, et [docs/API.md](docs/API.md).
- **Outils de développement** : [TOOLS.md](TOOLS.md).

## Fonctionnalités

### Hyperviseurs

- Enregistrer plusieurs hyperviseurs KVM : en local (`qemu:///system`) ou à distance par SSH
  (`qemu+ssh://…`). La connexion est vérifiée à l'ajout et à la modification.
- Donner l'accès à un hyperviseur à certains utilisateurs seulement.

### Machines virtuelles

- Lister les VM de chaque hyperviseur, avec leur état, leur mémoire et leurs processeurs.
- Créer une VM : mémoire, processeurs, disques, image ISO d'installation, réseau, BIOS ou
  UEFI.
- Démarrer, arrêter, cloner et supprimer une VM.
- Ouvrir la console graphique d'une VM dans le navigateur (noVNC), avec un jeton valable
  deux minutes.
- Suivre les opérations longues (création, clonage) comme des tâches en arrière-plan.

### Stockage et réseaux

- Lister, créer, agrandir et supprimer des disques.
- Lister et créer des pools de stockage et des réseaux virtuels.

### Templates

- Construire des images de VM avec **Packer**, sur l'hyperviseur lui-même.
- Garder chaque construction comme une version numérotée ; reconstruire, arrêter un build,
  lire ses logs en direct, supprimer une version.
- Créer des VM à partir d'un template en quelques secondes (copie du disque).
- Donner des droits sur un template à des utilisateurs ou à des groupes : l'**utiliser**
  (créer des VM) ou le **gérer** (le modifier et le reconstruire), avec un nombre maximum de
  VM par personne.

### Utilisateurs, groupes et droits

- Connexion par mot de passe, avec des jetons JWT courts et un renouvellement automatique de
  la session.
- Deux rôles : administrateur et utilisateur. Inscription libre activable par un
  administrateur.
- Gestion des comptes : création, désactivation, réinitialisation du mot de passe.
- **Quotas** par utilisateur : nombre de VM, processeurs, mémoire et espace disque.
- **Groupes** d'utilisateurs, qui peuvent contenir d'autres groupes. Chacun voit les groupes
  dont il fait partie.
- **Propriété des VM** : chaque utilisateur ne voit que ses VM et celles qu'on lui prête.
  Une VM peut être **prêtée** (console, démarrage, arrêt) ou **donnée** à un autre
  utilisateur.
- **Journal d'audit** : qui a fait quoi, consultable par les administrateurs.
- Protection contre les essais de mots de passe : compte bloqué 15 minutes après 5 échecs, et
  nombre de connexions limité par adresse IP.

### Interface web (`hypervisor-ui`)

- Tableau de bord, liste et détail des hyperviseurs, détail d'une VM avec sa console.
- Pages des templates : versions, logs de construction, droits.
- Espace d'administration : utilisateurs, groupes et journal d'audit.
- Page « Mon compte » : quota utilisé, groupes, changement de mot de passe.
- Une fausse API intégrée, pour développer l'interface sans hyperviseur.

### Déploiement et sécurité

- Déploiement complet avec Docker Compose : API, noVNC et interface.
- Tout passe en **HTTPS** par trois HAProxy (interface, API, consoles), avec votre
  certificat ; l'interface appelle l'API en HTTPS, en vérifiant son certificat.
- Documentation OpenAPI et interface Swagger.
- Tests automatisés (sans hyperviseur réel), et images Docker publiées sur Docker Hub par
  GitHub Actions.

## Auteurs

Projet réalisé par :

- Rémi Alban
- Taha Khetib
- Amaury Chau
- Julien Martageix
- Abdellatif Hsaini
