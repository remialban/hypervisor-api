# Passation API → front Angular (authentification, rôles, partage)

> Pour l'agent qui travaille sur `~/devProjects/hypervisor-ui`. Rédigé le 23 octobre 2026.

## À faire en premier : te renseigner

Ne te fie pas qu'à ce résumé. Avant d'écrire du code, lis dans le dépôt de l'API
(`~/devSTI/virt/hypervisor-api/hypervisor-api`, branche **`auth`** ; ne change pas de branche dans ce dépôt, quelqu'un d'autre
l'utilise — si `docs/AUTH.md` n'existe pas, c'est que la branche extraite n'est pas `auth` : préviens l'utilisateur) :

1. [`docs/AUTH.md`](AUTH.md) : le fonctionnement complet (connexion, rôles, matrice des droits, prêt, quotas, console, audit, variables d'environnement, conseils pour le front). **C'est la référence.**
2. [`docs/API.md`](API.md) : liste des routes.
3. Le contrat exact et à jour, généré par le code : `http://127.0.0.1:3000/swagger-ui/` ou
   `http://127.0.0.1:3000/api-docs/openapi.json` quand l'API tourne.
4. `src/auth_tests.rs` : les tests décrivent le comportement attendu cas par cas (qui peut quoi, quel code HTTP).
5. `docs/bruno/` : une requête d'exemple par route (dossiers `00-Auth`, `08-Users`, `09-Admin`, `10-Sharing`).

En cas de doute entre ce document et le code ou le Swagger, **le code a raison** : dis-le à l'utilisateur pour qu'il corrige ce document.

## Où en est l'API

**Fait, testé (119 tests) et vérifié sur le vrai binaire, y compris avec Docker :**

- Connexion par nom d'utilisateur et mot de passe, JWT d'accès + refresh token, endpoint au format OAuth2.
- Deux rôles (`admin`, `user`), accès par hyperviseur, propriétaire de chaque VM, prêt de VM, quotas optionnels, inscription libre activable par l'admin, journal d'audit.
- Console VNC protégée : jetons aléatoires de courte durée, vérifiés avec les droits sur la VM.
- Conteneurs : `docker-compose.yml` mis à jour (fusionné avec les ajouts de Rémi sur `main`), base persistée dans `./data`, noVNC branché sur un volume dédié aux seuls jetons, en lecture seule. Vérifié avec `docker compose up --build` : connexion, droits, prêt, console, expiration des jetons.

**Pas fait / à savoir :**

- **Le front n'est pas branché.** Tant qu'il n'envoie pas de Bearer, tout appel à `/api/*` (sauf `/api/auth/*`) répond `401`.
- **Pas de HTTPS** : choix assumé, c'est un POC.
- **La console n'a pas été testée avec un vrai QEMU.** Tout le chemin (jeton → fichier → websockify) l'a été avec un faux serveur VNC.
- Le code est sur la branche **`auth`** (à jour de `main`), **pas encore fusionné dans `main`**.
- Aucun e-mail : pas de « mot de passe oublié » en libre-service. Un admin réinitialise le mot de passe.
- **Pas de recherche d'utilisateurs pour un non-admin** : pour prêter une VM, l'utilisateur doit saisir le nom exact de l'emprunteur (`POST …/shares {"username"}`). `GET /api/users` est réservé aux admins.

## Lancer l'API pour développer

```bash
cd ~/devSTI/virt/hypervisor-api/hypervisor-api
ADMIN_PASSWORD='dev-password-1' JWT_SECRET='dev-secret-dev-secret-dev-secret-123' cargo run
```

- Écoute sur `http://127.0.0.1:3000`. **Vérifie d'abord que le port 3000 est libre** : si `target/debug/api` y tourne déjà, c'est l'API du mainteneur, branchée sur de vrais hyperviseurs (ta règle du `CLAUDE.md` du front s'applique : aucune action destructive dessus).
- Au premier démarrage sur une base vide, elle crée l'admin `admin` / `ADMIN_PASSWORD`. Base : `api.db` dans le dossier courant (ajoute `DATABASE_URL=sqlite:///chemin/dev.db?mode=rwc` pour en isoler une).
- Sans KVM, ajoute un hyperviseur factice : `POST /api/hypervisors {"name":"test","uri":"test:///default"}` (driver de test de libvirt, tout est en mémoire et disparaît au redémarrage de l'API).
- `proxy.conf.json` envoie déjà `/api` et `/ping` vers `:3000` : même origine en développement, **pas de CORS à configurer**. Si le front est servi ailleurs, l'API accepte `CORS_ALLOWED_ORIGINS=http://localhost:4200`.
- **Le mock du front (`mock-api/server.mjs`) ne connaît pas l'authentification** : il faudra l'adapter (login, `/api/me`, les nouveaux champs) pour que `npm run mock` reste utilisable.

## Le contrat en bref

Format d'erreur inchangé : `{"status":"error","content":"…"}`. **Exception** : un corps JSON mal formé ou
incomplet renvoie `422` en texte brut (message d'Axum), comme avant.
Toutes les dates sont des **secondes Unix** (entiers).

### Session

| Route | Corps | Réponse |
|---|---|---|
| `POST /api/auth/login` | `{"username","password"}` | `{"access_token","token_type":"Bearer","expires_in":900,"refresh_token"}` |
| `POST /api/auth/refresh` | `{"refresh_token"}` | même forme, **le refresh token envoyé est détruit** |
| `POST /api/auth/logout` | `{"refresh_token"}` | `204` |
| `GET /api/auth/config` (public) | | `{"registration_open": bool}` |
| `POST /api/auth/register` (public) | `{"username","password"}` | `201` + utilisateur, ou `403` si fermé |
| `GET /api/me` | | `{"id","username","role":"admin"\|"user","disabled","created_at","quota":{max_vms,max_vcpus,max_memory_mb,max_disk_gb},"usage":{vms,vcpus,memory_mb,disk_gb}}` |
| `PUT /api/me/password` | `{"current_password","new_password"}` | `204`, ou `401` si l'ancien est faux ; ferme les autres sessions |

Points qui piègent :

- **Le rôle n'est pas dans la réponse de login** : appeler `GET /api/me` juste après (c'est aussi lui qui donne le quota affichable).
- Le refresh token est **à usage unique**. Deux refresh en parallèle = le second échoue en `401` et déconnecte l'utilisateur. Il faut **un seul refresh à la fois** (les autres requêtes attendent son résultat), puis rejouer la requête d'origine une seule fois.
- Un `401` sur `/api/auth/login` veut dire « mauvais identifiants » (message identique que le nom existe ou non) ; sur les autres routes, « jeton absent, expiré ou compte désactivé ». `429` sur le login : trop d'échecs, patienter 15 minutes.
- Un compte désactivé ou un rôle changé prend effet immédiatement, même avec un JWT encore valide : un `401` ou `403` peut donc arriver à tout moment.
- Mot de passe : 8 à 128 caractères. Nom d'utilisateur : 3 à 32 caractères parmi lettres, chiffres, `.`, `_`, `-` (insensible à la casse).
- Conseil de stockage : access token en mémoire (un signal), refresh token en `sessionStorage` si on veut survivre à un rechargement.

### Droits : ce que le front doit refléter

Résumé ; la matrice complète est dans `AUTH.md`.

- **`403`** = « tu existes mais tu n'as pas le droit de faire ça » ; **`404`** = « tu n'as aucun droit sur cette ressource » (elle peut exister). Le front ne doit pas distinguer « n'existe pas » de « pas à toi ».
- Un `user` voit les hyperviseurs auxquels l'admin lui a donné accès **et** ceux qui hébergent une VM qu'il possède ou qu'on lui prête. Pour les non-admins, **`uri` est une chaîne vide**.
- Un `user` ne peut créer une VM que sur un hyperviseur où il a l'accès : sinon `403` sur `POST …/vms` et sur `GET …/vm-options`. Prévoir de masquer « Créer une VM » (ou de l'expliquer) quand `vm-options` répond `403`.
- Hyperviseurs (création, modification, suppression), disques, pools, réseaux, utilisateurs, paramètres, audit : **admin seulement** (`403` sinon). Le front doit cacher ces entrées de menu aux non-admins.
- **Chaque VM porte deux nouveaux champs** : `owner` (nom d'utilisateur, `null` si créée hors de l'API) et `access` : `"admin"`, `"owner"` ou `"shared"` (prêtée à l'utilisateur courant). Pour `shared`, **masquer Cloner et Supprimer** et le partage ; garder Démarrer, Arrêter et la Console.
- Lister les VM d'un hyperviseur que l'utilisateur ne voit pas renvoie `404` (pas une liste vide).
- Créer une VM pour quelqu'un d'autre : champ optionnel `owner_id` dans `CreateVm`, admin seulement (`403` sinon).
- **Quota dépassé** : `403` avec un message qui nomme la limite (`Quota exceeded: max_vms is limited to 3 (4 requested)`). Pas de code d'erreur dédié.
- Cloner : le clone appartient à celui qui le demande et compte dans son quota.

### Console (changement important)

`GET /api/hypervisors/{h}/vms/{vm}/vnc` demande maintenant un Bearer et renvoie un jeton **aléatoire qui expire**
(120 s par défaut) :

```json
{ "hypervisor_id": "…", "vm_id": "…", "uuid": "…", "host": "…", "port": "5900",
  "token": "…64 caractères…", "novnc_url": "http://…/vnc.html?path=websockify/?token=…&autoconnect=true",
  "status": "connected", "expires_at": 1792737911 }
```

- Ouvrir `novnc_url` **tout de suite** après l'appel (nouvel onglet ou iframe) ; ne pas le garder en cache. Pour se reconnecter, rappeler la route.
- Nouveau champ `expires_at` : à ajouter à `vnc.model.ts` (qui n'a pas non plus `uuid`).
- `409` si la VM est arrêtée (« port VNC pas encore attribué ») ; `404` sans droit ou sans port VNC.
- **Les erreurs de cette route ont maintenant le format commun `{status, content}`** (avant : `{status, message}`).
- Le navigateur ouvre noVNC directement sur le port 6080 : aucun Bearer n'y est envoyé, c'est le jeton de l'URL qui fait foi.

### Prêt de VM et propriétaire

| Route | Qui | Notes |
|---|---|---|
| `GET …/vms/{vm}/shares` | propriétaire, admin | `[{"user_id","username","created_at","expires_at"}]` |
| `POST …/vms/{vm}/shares` | propriétaire, admin | `{"username","expires_at"?}` → `201`. `404` nom inconnu, `400` à soi-même ou date passée, `409` VM sans propriétaire |
| `DELETE …/vms/{vm}/shares/{user_id}` | propriétaire, admin, **ou l'emprunteur lui-même** | `204`. L'emprunteur peut « rendre » la VM avec son propre `user_id` |
| `PUT …/vms/{vm}/owner` | admin | `{"owner_id"}` → la VM. C'est aussi comment une VM créée hors de l'API reçoit un propriétaire |

L'emprunteur : console, démarrer, arrêter. Rien d'autre, il ne peut pas re-prêter.

### Écrans d'administration à prévoir

| Besoin | Routes |
|---|---|
| Liste / création / modification / suppression des comptes | `GET/POST /api/users`, `GET/PATCH/DELETE /api/users/{id}` (`PATCH {"role","disabled"}`) |
| Réinitialiser un mot de passe | `PUT /api/users/{id}/password {"new_password"}` |
| Quotas (remplacement complet, `null` = illimité) | `PUT /api/users/{id}/quota {"max_vms","max_vcpus","max_memory_mb","max_disk_gb"}` |
| Ouvrir / fermer les inscriptions | `GET/PUT /api/settings {"registration_open": bool}` |
| Qui peut créer des VM sur un hyperviseur | `GET /api/hypervisors/{id}/access`, `PUT/DELETE /api/hypervisors/{id}/access/{user_id}` |
| Journal d'audit | `GET /api/audit?limit=&offset=&user_id=&action=` (plus récent d'abord) |

Garde-fous côté API à afficher proprement : `409` si on retire le dernier admin actif, ou si on supprime un utilisateur
qui possède encore des VM.

## Ce qu'il faut changer dans le front (point de départ)

Constaté en lisant `src/app` : `provideHttpClient(withInterceptors([errorInterceptor]))` est le seul intercepteur, et le
catalogue d'erreurs ne connaît ni `401`, ni `403`, ni `429`.

1. **Service d'authentification** : `login`, `logout`, `refresh` (un seul à la fois), signal de l'utilisateur courant (`/api/me`).
2. **Intercepteur d'authentification**, placé *avant* `errorInterceptor` : ajoute le Bearer aux appels `/api` (pas à `/api/auth/login|refresh|register|config`), sur `401` fait un refresh puis rejoue la requête une fois, sinon retour à `/login`.
3. **Gardes de routes** : connecté / admin. Page `/login` ; lien « créer un compte » seulement si `GET /api/auth/config` le permet.
4. **Mapper d'erreurs** (`hypervisor-api-error-mapper.ts`) : ajouter les codes métier pour `401` (session expirée), `403` (droit manquant, ou quota si le message contient `Quota exceeded`), `429` (trop d'essais).
5. **Modèles** : `VirtualMachine` (+ `owner`, `access`), `CreateVm` (+ `owner_id?`), `VncResponse` (+ `uuid`, `expires_at`), `Hypervisor.uri` peut être vide ; nouveaux modèles `User`, `Quota`, `Usage`, `Share`, `AuditEntry`, `Settings`.
6. **Écrans** : prêt de VM (liste des emprunteurs, ajout par nom, retrait), administration (tableau ci-dessus), affichage du propriétaire et de l'usage de quota.
7. **Mock** (`mock-api/server.mjs`) à aligner sur ce contrat.

Respecte les règles du `CLAUDE.md` du front (structure classique, trois fichiers par composant, pas de HTML dans les pages métier,
explications pour débutants, français).

## Quand tu as un doute

Demande à l'utilisateur plutôt que de deviner un comportement de l'API, ou lis `src/auth_tests.rs` : chaque règle y a un test
nommé en toutes lettres (par exemple `a_borrower_gets_the_console_and_power_but_nothing_else`).
