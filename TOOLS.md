# Outils de développement

Les outils utilisés pour développer le projet, côté API (`hypervisor-api`) et côté interface
(`hypervisor-ui`), et à quoi sert chacun.

## API (Rust)

### Langage et bibliothèques

| Outil | À quoi il sert |
|---|---|
| **Rust** (édition 2024, 1.88 ou plus) et **Cargo** | Le langage de l'API, et son outil de compilation et de gestion des dépendances (`Cargo.toml`). |
| **Axum** et **Tokio** | Le serveur web (routes, extracteurs, réponses JSON) et le moteur asynchrone sur lequel il tourne. |
| **virt** (libvirt) | Parle aux hyperviseurs : lister, créer, démarrer les VM, gérer disques et réseaux. Fonctionne en local ou par SSH (`qemu+ssh://`). |
| **roxmltree**, **xmltree** | Lisent et écrivent le XML de libvirt (description des VM, disques, réseaux). |
| **SQLx** et **PostgreSQL** | La base de données : comptes, droits, groupes, templates, audit. Le schéma est dans `migrations/` et s'applique tout seul au démarrage. |
| **utoipa** et **Swagger UI** | Génèrent la documentation OpenAPI à partir du code, et l'interface Swagger (`/swagger-ui/`). |
| **argon2** | Hache les mots de passe. |
| **jsonwebtoken** | Crée et vérifie les jetons de connexion (JWT). |
| **tower-http** | Le CORS, pour un front servi sur une autre origine. |
| **sysinfo** | Lit les ressources de la machine (processeurs, mémoire). |

### Qualité du code

| Commande | À quoi elle sert |
|---|---|
| `cargo fmt` | Met le code en forme selon le style standard de Rust. `cargo fmt --check` vérifie sans rien modifier. |
| `cargo clippy --all-targets --all-features -- -D warnings` | Analyse le code et signale les erreurs courantes et le code à simplifier. Aucun avertissement n'est toléré. |
| `cargo test` | Lance les tests. Ils n'ont besoin d'aucun hyperviseur : ils utilisent le pilote de test de libvirt (`test:///default`, en mémoire). Ils ont besoin d'un PostgreSQL jetable (`TEST_DATABASE_URL`), où chaque test crée son propre schéma vide (voir [INSTALLATION.md](INSTALLATION.md#5-tests-et-qualité)). |

À lancer avant chaque commit :

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

### Tester l'API à la main

| Outil | À quoi il sert |
|---|---|
| **Swagger UI** | Sur `/swagger-ui/` : essayer chaque route depuis le navigateur, avec le bouton **Authorize** pour le jeton. |
| **Bruno** | Une collection de requêtes, une par route, dans `docs/bruno/` (voir [docs/BRUNO.md](docs/BRUNO.md)). Le script `docs/bruno/check-sync.sh` vérifie qu'elle couvre toutes les routes de l'OpenAPI. |
| **curl** et **jq** | Appeler l'API depuis le terminal et lire ses réponses JSON. |

## Virtualisation

| Outil | À quoi il sert |
|---|---|
| **KVM** et **QEMU** | La virtualisation elle-même, sur les hyperviseurs. |
| **libvirt** et **virsh** | Le service qui gère les VM sur chaque hyperviseur, et sa ligne de commande, pratique pour vérifier ce que fait l'API. |
| **Packer** et **qemu-img** | Construisent les images des templates : Packer installe un système dans une VM temporaire, `qemu-img` prépare le disque. Lancés sur l'hyperviseur. |
| **noVNC** et **websockify** | La console des VM dans le navigateur : noVNC est le client web, websockify relaie son WebSocket vers le port VNC de la VM. |

## Interface web (Angular)

| Outil | À quoi il sert |
|---|---|
| **Node.js 22** et **nvm** | L'environnement JavaScript. `nvm use` choisit la bonne version, lue dans `.nvmrc`. |
| **npm** | Installe les dépendances (`npm install`) et lance les scripts du projet. |
| **Angular 21** et **Angular CLI** | Le framework de l'interface, et son outil : `npm start` (`ng serve`) lance le serveur de développement sur `http://localhost:4200`, `npm run build` compile pour la production. |
| **TypeScript** | Le langage de l'interface : du JavaScript typé. |
| **RxJS** | Les appels à l'API et les flux de données asynchrones. |
| **Bootstrap 5** et **Angular CDK** | La mise en page, les composants de base (modales, listes…). |
| **Prettier** | Met le code de l'interface en forme. |
| **Fausse API** (`mock-api/`) | Une imitation de l'API en Node, avec les mêmes routes : `npm run mock` ou `npm run mock:full` (avec des VM d'exemple), pour développer l'interface sans hyperviseur. |
| `proxy.conf.json` | En développement, fait passer `/api` du serveur Angular (4200) à l'API (3000), pour éviter les problèmes de CORS. |
| `npm test` | Lance les tests de l'interface (`ng test`). |

## Conteneurs et déploiement

| Outil | À quoi il sert |
|---|---|
| **Docker** | Emballe chaque partie dans une image : l'API (compilée dans `rust:trixie`, lancée dans `debian:trixie-slim`), noVNC, et l'interface (compilée avec Node, servie par nginx). |
| **Docker Compose** | Lance tous les conteneurs d'un projet avec leurs réseaux, volumes et variables (`docker-compose.yml`, `.env`). |
| **HAProxy** | Les trois portes d'entrée HTTPS (interface, API, consoles). Voir [docs/HAPROXY.md](docs/HAPROXY.md). |
| **nginx** | Sert l'application Angular compilée et relaie `/api` vers l'API. |
| **OpenSSL** | Crée les certificats auto-signés pour essayer en local. |
| **PostgreSQL** (image `postgres:17-alpine`) | La base de l'API, dans son propre conteneur. `pg_dump` et `pg_restore` la sauvegardent et la restaurent. |
| **sqlite3** | Seulement pour reprendre les données d'une ancienne installation SQLite (`scripts/sqlite-to-postgres.sh`). |

L'architecture complète est décrite dans [docs/DOCKER.md](docs/DOCKER.md).

## Collaboration

| Outil | À quoi il sert |
|---|---|
| **Git** et **GitHub** | Le code des deux dépôts, les branches et les pull requests. |
| **GitHub Actions** | Construit les images Docker et les publie sur Docker Hub à chaque push sur `main` et à chaque tag de version (`v1.2.3`). |
| **Docker Hub** | Héberge les images publiées, pour déployer sans tout recompiler. |
| **Mermaid** | Les schémas de la documentation, écrits en texte dans les fichiers Markdown. `mmdc` (Mermaid CLI) vérifie qu'ils s'affichent. |
