# Implementation Plan

## Objectif

Construire une API Rust/Axum capable de gerer plusieurs hyperviseurs KVM via libvirt, avec une documentation OpenAPI et un stockage persistant limite aux hyperviseurs.

## Regle de stockage

SQLite stocke uniquement les informations de connexion des hyperviseurs :

- `id`
- `nom`
- `uri`
- `statut`

Les VM, disques et reseaux ne doivent pas etre stockes dans SQLite. Leur source de verite sera libvirt.

## Etat actuel

### Termine

- [x] Projet Rust initialise avec Axum et Tokio.
- [x] Organisation modulaire avec `main.rs`, `routes.rs`, `state.rs`, `models.rs`, `hypervisors.rs`, `vms.rs`, `infrastructure.rs`, `common.rs` et `docs.rs`.
- [x] Connexion a SQLite via SQLx.
- [x] Creation automatique de la table SQLite `hypervisors`.
- [x] CRUD SQLite des hyperviseurs : liste, lecture, ajout, modification et suppression.
- [x] Stockage de l'URI libvirt de chaque hyperviseur.
- [x] Verification de connexion avec `virt::connect::Connect` et `is_alive()`.
- [x] Mise a jour du statut `connecte` ou `deconnecte` lors des lectures.
- [x] Rejet d'une URI libvirt inaccessible avec une reponse `502`.
- [x] Routes VM conservees comme contrat API sans ecriture SQLite.
- [x] Routes disques conservees comme contrat API sans ecriture SQLite.
- [x] Routes reseaux conservees comme contrat API sans ecriture SQLite.
- [x] Route VNC exposee comme point d'extension.
- [x] Handler VNC isole dans `vnc.rs` pour permettre un developpement independant.
- [x] Documentation OpenAPI generee avec Utoipa.
- [x] Interface Swagger disponible sur `/swagger-ui/`.
- [x] README d'installation et de demarrage.
- [x] Documentation des endpoints dans `docs/API.md`.
- [x] Compilation verifiee avec `cargo check`.
- [x] Identifiants, champs, fonctions, messages et descriptions OpenAPI du code source passes en anglais.
- [x] Workflow GitHub Actions ajoute pour verifier chaque `push` et `pull_request`.
- [x] Type d'erreur `ApiError` (`errors.rs`) avec conversion des erreurs libvirt en codes HTTP.
- [x] Module `libvirt.rs` : lecture de l'URI depuis SQLite, `list_vms` et `get_vm` (branches dans `vms::list` et `vms::get`).

## Reste a faire

### Priorite 1 : libvirt pour les VM

- [x] Creer un service libvirt dedie, separe des handlers HTTP (`libvirt.rs`, a transformer en trait pour les tests).
- [x] Ouvrir la connexion vers l'hyperviseur selectionne depuis son URI SQLite (`hypervisor_uri` + `Connect::open`, a coordonner avec le Developpeur 1).
- [x] Lister les VM avec libvirt (service fait, handler `vms::list` branche, teste sur qemu+ssh).
- [x] Lire les details d'une VM (service fait, handler `vms::get` branche, teste sur qemu+ssh).
- [ ] Creer une VM a partir d'une definition XML valide.
- [x] Demarrer une VM avec `Domain::create` (teste sur qemu+ssh).
- [x] Arreter proprement une VM avec `Domain::shutdown` (asynchrone, teste).
- [x] Ajouter l'arret force avec `Domain::destroy` (`?force=true`, teste).
- [ ] Cloner une VM avec une nouvelle definition XML et ses disques.
- [ ] Supprimer une VM avec confirmation explicite.
- [ ] Ajouter des tests avec un faux service libvirt ou une couche mockable.

### Priorite 2 : disques et reseaux libvirt

- [ ] Lister les volumes libvirt sans les copier dans SQLite.
- [ ] Creer un volume avec le pool de stockage choisi.
- [ ] Modifier les metadonnees ou la taille selon les capacites libvirt.
- [ ] Supprimer un volume avec controle des dependances VM.
- [ ] Lister les reseaux libvirt.
- [ ] Creer, demarrer et arreter un reseau libvirt.
- [ ] Supprimer un reseau avec verification qu'il n'est pas utilise.

### Priorite 3 : VNC

- [ ] Lire le display VNC de la VM depuis libvirt.
- [ ] Retourner une URL ou un port VNC securise.
- [ ] Ajouter une authentification et une autorisation par hyperviseur.
- [ ] Ne jamais exposer directement un port VNC sans controle d'acces.
- [ ] Ajouter un proxy WebSocket si l'interface web doit utiliser noVNC.

### Priorite 4 : securite et robustesse

- [ ] Ajouter une authentification API.
- [ ] Ajouter une autorisation par role et par hyperviseur.
- [ ] Valider les noms, URI, tailles et formats recus.
- [ ] Ajouter des timeouts pour les connexions libvirt.
- [ ] Ne pas retourner de secrets ou d'informations sensibles dans les erreurs.
- [ ] Ajouter des logs structures et un identifiant de requete.
- [ ] Ajouter une gestion propre des conflits et des hyperviseurs indisponibles.

### Priorite 5 : qualite et exploitation

- [ ] Installer `rustfmt` localement et appliquer `cargo fmt`.
- [ ] Ajouter des tests unitaires pour les modeles et services.
- [ ] Ajouter des tests HTTP pour chaque route.
- [ ] Ajouter un environnement de test libvirt ou des mocks.
- [ ] Ajouter une configuration par variables d'environnement.
- [ ] Remplacer l'URI SQLite en dur par `DATABASE_URL`.
- [ ] Ajouter des migrations SQL versionnees.
- [ ] Ajouter Docker ou une procedure de deploiement.
- [ ] Ajouter une supervision et un endpoint de sante libvirt.

## Organisation du travail a trois

### Developpeur 1 : hyperviseurs et fondation

- `hypervisors.rs`
- `state.rs`
- migrations SQLite
- service de connexion libvirt
- authentification et autorisation

### Developpeur 2 : ressources libvirt

- `vms.rs`
- service VM

### Developpeur 3 : infrastructure et VNC

- `infrastructure.rs`
- `vnc.rs`
- service disques
- service reseaux
- VNC

### Fichiers a coordonner

- `routes.rs`
- `docs.rs`
- `models.rs`
- `README.md`
- `docs/API.md`
- `implementation_plan.md`

Avant toute modification d'un fichier partage, prevenir l'autre developpeur et mettre a jour ce plan.

## Validation a chaque modification

Apres chaque changement de code :

```bash
cargo check
cargo test
```

Le workflow GitHub Actions `.github/workflows/ci.yml` execute automatiquement ces controles sur chaque `push` et `pull_request`, avec en plus Clippy en mode `-D warnings`.

Quand `rustfmt` est installe :

```bash
cargo fmt --check
```

Apres toute modification d'une route :

1. mettre a jour les annotations OpenAPI ;
2. mettre a jour `docs/API.md` ;
3. ajouter ou actualiser un exemple `curl` ;
4. mettre a jour la section correspondante de ce plan.

## Derniere validation connue

- `cargo check` : reussi.
- `cargo fmt --check` : non disponible tant que `rustfmt` n'est pas installe.
- Les routes VM, disques, reseaux et VNC repondent actuellement `501` et ne persistent aucune donnee SQLite.
