# BRNavi backend starter

## English

This starter runs the JSON-RPC-2 gateway and its in-process load balancer in
front of two identical BRNavi backend instances. The gateway and backend
protocol follow the upstream projects:

- [JSON-RPC-2-GATEWAY](https://github.com/vsvictor/JSON-RPC-2-GATEWAY)
- [JSON-RPC-2](https://github.com/vsvictor/JSON-RPC-2)

The backend binary uses the upstream `json-rpc-2-backend` crate. Each instance
registers the crate's starter methods (`health`, `echo`, `identify`, and
`rpc.list_methods`); `identify` returns its instance ID so requests can be
observed across the pool.

### CarAPI plugin

BRNavi installs the pinned [`json-rpc-2-car`](https://github.com/vsvictor/JSON-RTC-2-Car/tree/main/crates/json-rpc-2-car)
plugin during backend startup. `BackendBuilder::from_dotenv()` loads local
`.env` values before `CarConfig::from_env()` reads `CARAPI_TOKEN` and
`CARAPI_SECRET` and passes the configuration to the plugin. For systemd
deployments, provide the same variables through `/etc/brnavi/backend.env`.
Keep credentials out of tracked files; `.env.example` contains empty values
only.

For native systemd services, the project's `.env` is not loaded automatically:
the service has no project working directory and `ProtectHome=true`. When
changing `CARAPI_TOKEN` or `CARAPI_SECRET` in `.env`, securely synchronize those
two values to `/etc/brnavi/backend.env` (root-owned, mode `0600`) and restart
both `brnavi-backend@1` and `brnavi-backend@2`. Preserve all other environment
values; never upload the complete development `.env` over the production file.

All `car.*` methods require a valid BRNavi access token (`access_token` or
`authorization: "Bearer …"`). The server validates and removes the credential
before forwarding business parameters to the CarAPI plugin; unknown business
parameters remain rejected. The plugin's CarAPI credentials are separate from
the user's BRNavi token.

### Плагін CarAPI

BRNavi встановлює плагін [`json-rpc-2-car`](https://github.com/vsvictor/JSON-RTC-2-Car/tree/main/crates/json-rpc-2-car)
під час запуску бекенда. `BackendBuilder::from_dotenv()` завантажує значення з
локального `.env`, після чого `CarConfig::from_env()` читає `CARAPI_TOKEN` і
`CARAPI_SECRET` та передає конфігурацію плагіну. Для systemd задайте ті самі
змінні в `/etc/brnavi/backend.env`. Не зберігайте облікові дані у файлах під
контролем Git; у `.env.example` наведені лише порожні значення.

Для нативних служб systemd файл `.env` проєкту не завантажується автоматично:
служба не має робочого каталогу проєкту, а `ProtectHome=true` закриває домашній
каталог. Після зміни `CARAPI_TOKEN` або `CARAPI_SECRET` у `.env` безпечно
синхронізуйте лише ці два значення до `/etc/brnavi/backend.env` (власник root,
права `0600`) і перезапустіть `brnavi-backend@1` та `brnavi-backend@2`.
Збережіть решту налаштувань; не замінюйте виробничий файл повним локальним `.env`.

Усі методи `car.*` вимагають чинний токен доступу BRNavi (`access_token` або
`authorization: "Bearer …"`). Сервер перевіряє та вилучає токен перед передаванням
бізнес-параметрів плагіну CarAPI; невідомі бізнес-параметри й надалі відхиляються.
Облікові дані CarAPI плагіну відокремлені від токена користувача BRNavi.

### Garage persistence

Authenticated RPC methods use the existing auth plugin's verified subject and
the already configured auth/admin MongoDB database. No additional database
credentials or dependencies are needed. The collection name is exactly
`garage`. Caller-supplied `owner`, `user_id`, verification, timestamps, and other
unknown RPC parameters are rejected; access credentials are removed by
`authorize` and never stored.

- `garage.vehicles.add` parameters:
  `{access_token, class_id?: "passenger", make_id: string, model_id: string,
  year: integer, trim_id?: string, vehicle: object}`.
  Any nonempty legacy `class_id` is retained; omission defaults to `passenger`.
  The snapshot requires `make` and `model` objects with nonempty string `id`
  and `name` and object `raw_data`. `trim` and `trim_id` are optional; when
  supplied, both must contain matching nonempty IDs and the trim must include
  a nonempty name and object `raw_data`. Without a trim, omit `trim_id` and
  omit `trim` or use null. IDs and positive integer year must match the parameters.
  `bodies`, `engines`, `mileages`,
  `exterior_colors`, and `interior_colors` must be arrays of objects (empty
  arrays are valid). Optional `selected_exterior_color`,
  `selected_interior_color`, `vin_data`, and `equipment_data` are objects or
  null; optional `vin` is a string or null. Unknown snapshot fields and all raw
  JSON are retained, not flattened, including literal Extended JSON objects.
  Nested credential keys (`access_token`, `refresh_token`, `authorization`,
  `token`, `password`, case-insensitive) and NUL-containing keys are rejected. Integers
  must fit BSON's signed 64-bit representation.
- Add returns `{vehicle: record}`, where `record` is
  `{id: string, name: "<make.name> <model.name> · <year>", category: class_id,
  verification: "none", vehicle: <original snapshot object>}`.
  ID and creation timestamps are generated by the server. No dimensions or
  weight are inferred from unverified API values; optional
  `dimensions: {height_m, width_m, length_m}` and `weight_kg` are currently
  omitted.
- `garage.vehicles.list` parameters: `{access_token}`. Returns
  `{vehicles: [record, ...], active_vehicle_id: string | null}`.
- `garage.vehicles.set_active` parameters:
  `{access_token, vehicle_id: string}`. Returns `{active_vehicle_id: string}`.
  Missing and foreign-owned vehicle IDs both return invalid params; selecting
  the current vehicle again succeeds.
- `garage.vehicles.update` parameters: `{access_token, vehicle_id: string,
  class_id?: "passenger", make_id: string, model_id: string, year: integer,
  trim_id?: string, vehicle: object}`. The same validation and default as add
  apply. Replaces the snapshot, name and category, retaining ID, creation time,
  verification and active selection. Returns `{vehicle: record}`.
- `garage.vehicles.delete` parameters: `{access_token, vehicle_id: string}`.
  Returns `{active_vehicle_id: string | null}` after deletion. Both operations
  return `-32602` for a missing or foreign-owned ID.

Each owner has a single garage document, keyed by the verified auth subject
using MongoDB's built-in unique `_id` index. Vehicle snapshots are structured
BSON subdocuments, not JSON strings. A single atomic update appends a vehicle
and makes the first saved car active, including after the garage becomes empty;
concurrent initial saves retry duplicate
owner upserts without changing an existing selection. List order is atomic
append order. Selection is one atomic owner-and-contained-vehicle-scoped update,
so concurrent saves/selections cannot create multiple active cars or select
another owner's car. Update and deletion are also atomic and owner-scoped.
Deleting the active car selects the first remaining car in list order, or null
if none remain; deleting another car preserves the selection. No additional
indexes are necessary for these operations.
The aggregate garage is subject to MongoDB's 16 MiB document limit; exceeding it
fails the whole update without a partial save. RPC errors are `-32001` for
authentication, `-32602` for invalid input/selection, and `-32603` for storage
failures, without raw storage errors in responses or logs.

Validation: `cargo test garage:: --quiet`, `cargo build --quiet`, and
`cargo fmt --check`. Garage tests use two real authenticated SQLite-backed auth
users with an isolated fake garage store, covering ownership, authentication,
snapshot preservation, validation, concurrency, and storage errors. Auth test
files are created under the project and removed automatically.

The opt-in live adapter test lives separately in `src/garage/storage/tests.rs`:
`GARAGE_TEST_MONGODB_URI='mongodb://127.0.0.1:27017/?directConnection=true' cargo test garage::storage::tests::mongo_adapter_end_to_end -- --ignored --nocapture`.
Use only an explicitly approved test MongoDB server. The test never loads app
configuration or prints the URI. It generates a unique
`brnavi_garage_test_<ObjectId>` database, checks its name before dropping it,
and cleans up even when exercise assertions panic. It never lists databases
or touches existing database names. It tests two-owner isolation, first-active
selection, stable append order, persistence through a newly constructed adapter,
raw snapshot/equipment preservation, editing and deletion, concurrent saves,
selection and deletion, malformed stored data, and oversized writes rejected
without changing the garage. Standard tests leave this live test ignored.

Live validation succeeded on a dedicated loopback-only MongoDB process using a
fresh project-local data directory and a unique throwaway database; the database
was dropped and the process/data directory removed afterward. Default local
port 27017 was unavailable; no shared service was installed or restarted.
The live test exposed and fixed literal Extended JSON decoding: adapter reads
use `RawDocumentBuf::to_document()` rather than Serde interpreting raw `$date`
or `$oid` API objects as BSON scalar types. Stored records are validated on
write/read; corrupt records and dangling active IDs return a generic internal
error, not invalid public data.

### Збереження гаража

Автентифіковані RPC використовують перевірений `subject` наявного auth-плагіну
та вже налаштовану MongoDB базу auth/admin. Колекція називається саме `garage`;
нові облікові дані й залежності не потрібні. Передані клієнтом `owner`,
`user_id`, статус перевірки, часові мітки та невідомі RPC-параметри
відхиляються. `authorize` вилучає токен, який не зберігається.

- `garage.vehicles.add`: `{access_token, class_id?: "passenger",
  make_id: string, model_id: string, year: integer, trim_id?: string,
  vehicle: object}`. Непорожній попередній `class_id` зберігається без змін;
  за відсутності використовується `passenger`. У `vehicle` обов'язкові
  `make`, `model` із непорожніми рядковими `id`, `name` та об'єктом
  `raw_data`. Комплектація необов'язкова: `trim_id` можна опустити, а `trim`
  опустити або передати null. Якщо комплектацію задано, обидва поля обов'язкові,
  ID мають збігатися, а `trim` містить непорожню назву й об'єкт `raw_data`.
  Ідентифікатори й додатний цілий рік мають збігатися з
  RPC-параметрами. `bodies`, `engines`, `mileages`, `exterior_colors`,
  `interior_colors` — масиви об'єктів, зокрема порожні. Необов'язкові
  `selected_exterior_color`, `selected_interior_color`, `vin_data`,
  `equipment_data` — об'єкти або null; `vin` — рядок або null.
  Невідомі поля snapshot і вкладений JSON зберігаються без перетворення
  структури, включно з буквальними Extended JSON об'єктами.
  Вкладені ключі `access_token`, `refresh_token`, `authorization`, `token`, `password`
  (без урахування регістру) та ключі з NUL відхиляються. Цілі числа мають
  уміщуватися у знаковий 64-бітний BSON.
- Результат add: `{vehicle: record}`, де record містить
  `{id: string, name: "<make.name> <model.name> · <year>", category: class_id,
  verification: "none", vehicle: <оригінальний snapshot>}`.
  Ідентифікатор, власника й часові мітки визначає сервер. Необов'язкові
  `dimensions: {height_m, width_m, length_m}` та `weight_kg` наразі відсутні:
  неперевірені дані API не використовуються для припущень про одиниці виміру.
- `garage.vehicles.list`: `{access_token}` →
  `{vehicles: [record, ...], active_vehicle_id: string | null}`.
- `garage.vehicles.set_active`: `{access_token, vehicle_id: string}` →
  `{active_vehicle_id: string}`. Чужий або відсутній автомобіль повертає
  однакову помилку параметрів; повторний вибір поточного дозволений.
- `garage.vehicles.update`: `{access_token, vehicle_id: string,
  class_id?: "passenger", make_id: string, model_id: string, year: integer,
  trim_id?: string, vehicle: object}` → `{vehicle: record}`. Правила перевірки
  та значення за замовчуванням ті самі, що для add. Замінює snapshot, назву
  й категорію, зберігаючи ID, час створення, verification і активний вибір.
- `garage.vehicles.delete`: `{access_token, vehicle_id: string}` →
  `{active_vehicle_id: string | null}` після видалення. Обидві операції
  повертають `-32602` для чужого або відсутнього ID.

Для кожного власника є один документ із `_id`, рівним перевіреному subject,
і стандартним унікальним індексом MongoDB. Snapshot зберігається як BSON
документ, не JSON-рядок. Один атомарний update додає автомобіль і робить
перший активним, зокрема після спорожнення гаража. Конкурентне створення документа повторює update після
конфлікту унікальності, не змінюючи наявний вибір. Порядок списку відповідає
порядку атомарного додавання. Зміна вибору атомарно перевіряє власника та
наявність автомобіля. Редагування й видалення атомарні та обмежені власником.
Видалення активного автомобіля вибирає перший із решти в порядку списку або
null, якщо гараж порожній; видалення іншого не змінює вибір. Додаткові індекси
не потрібні. Діє обмеження MongoDB
16 MiB на весь гараж; перевищення скасовує update повністю. Помилки RPC:
`-32001` — автентифікація, `-32602` — дані або вибір, `-32603` — сховище,
без необроблених повідомлень БД у відповідях чи логах.

Перевірка: `cargo test garage:: --quiet`, `cargo build --quiet`,
`cargo fmt --check`. Тести використовують двох справді автентифікованих
користувачів SQLite auth та ізольоване fake-сховище гаража: перевіряють
ізоляцію, snapshot, некоректні дані, конкурентність і помилки сховища.
Тестові файли auth створюються в проєкті й автоматично видаляються.

Окремий opt-in тест справжньої MongoDB — у `src/garage/storage/tests.rs`:
`GARAGE_TEST_MONGODB_URI='mongodb://127.0.0.1:27017/?directConnection=true' cargo test garage::storage::tests::mongo_adapter_end_to_end -- --ignored --nocapture`.
Використовуйте лише дозволений тестовий сервер. Тест не читає конфігурацію
застосунку й не друкує URI. Створює лише унікальну базу
`brnavi_garage_test_<ObjectId>`, перевіряє назву перед видаленням і прибирає її
навіть після panic у перевірках. Не переглядає список баз і не торкається
наявних баз. Перевіряє двох власників, перший активний автомобіль, порядок,
збереження після створення нового адаптера, snapshot/equipment, редагування й
видалення, конкурентні збереження, вибір та видалення, пошкоджені дані й
відхилення надмірного запису без зміни
гаража. У звичайному запуску тест ігнорується.

Live-перевірка успішна на окремому MongoDB процесі лише на loopback зі свіжим
каталогом даних у проєкті й унікальною тестовою базою. Базу видалено, процес
зупинено, каталог прибрано. Порт 27017 був недоступний; спільні служби не
встановлювалися й не перезапускалися. Live-тест виявив і виправив проблему
буквальних Extended JSON ключів: читання використовує
`RawDocumentBuf::to_document()`, щоб `$date`/`$oid` у raw API об'єктах не
перетворювалися Serde на BSON скаляри. Записи перевіряються перед збереженням
і після читання; пошкоджені записи чи активний ID поза гаражем повертають
узагальнену внутрішню помилку.

### Login tracking (CoopNav)

The backend installs the [ExServerTracker](../ExServerTracker) `CoopNavPlugin`
and exposes a host method that bridges the login device identity to a tracking
session. Stage 2 is off unless `COOPNAV_STAGE2=true` on both backend units with
the same `COOPNAV_EPOCH`, `COOPNAV_COOP_BRAND` and `COOPNAV_REDIS_URL`. Stage 3
stays disabled. No new device ID is created: the `deviceId` the client already sends
to `auth.login`/`auth.loginWithGoogle` becomes the tracker ID.

`tracking.open` — params `{access_token | authorization: "Bearer …", deviceId}`
(unknown fields rejected; `deviceId` is 1–128 chars of `[A-Za-z0-9._:-]`).
Initial authorization is `auth.authorize(params, &[])` (valid token, no extra scope).
The server then requires that the verified subject has a current OAuth device
session (`sessions` row with the same `user_id` and `device_id`, fresh
`last_seen`). It then idempotently adds the fixed `system.xrnavi.tracking`
role to that owner, including existing users, and checks both tracking
permissions using the normal OAuth authorization API. Result:

```json
{"trackerId": "<deviceId>", "sessionId": "<32 hex>", "driverId": "<subject>", "brandId": "personal:<subject>"}
```

`brandId` is a personal tracking namespace, not a fleet: each user only ever
writes into `personal:<own subject>`. `sessionId` is 128 random bits, stable
for the same (subject, device, OAuth login) and replaced after a new login.
Errors: `-32011` device not registered for this user / session no longer
valid, `-32602` invalid params. The client then calls
`tracking.ingest {sessionId, driverId, brandId, points[1..100]}` and
`serverTime {sessionId}`; every call re-checks that the grant's OAuth device
session is still the same and not idle for more than 30 days.

Storage (auth database): `tracking_sessions` (`_id: {subject, device_id,
auth_sid}`, unique `session_id`, index created at startup) and
`tracker_points` (`_id: {brand_id, driver_id, point_id}`; duplicate identical
points are idempotent, different payloads with the same ID are conflicts).
Redis is not opened while `COOPNAV_STAGE2` is false. With the flag on, both
units share one local Redis donor index (`coopnav:v1`); Mongo history stays
`personal:<subject>`.

Automatic permissions and client retry:
- Startup writes the reserved `system.xrnavi.tracking` role in the existing
  OAuth `roles` collection to exactly `tracking.ingest` and
  `tracking.server_time`, plus `coop.sync_peers` when `COOPNAV_STAGE2=true`.
  `tracking.open` uses the verified subject's ObjectId
  to `$addToSet` this role in `users.roles`; other roles are preserved. No
  client-selected role/user, wildcard, or permission bypass is used.
  New users and existing users are provisioned lazily on their first valid open.
- Existing access tokens are immutable. If any permission of the current list is absent,
  `tracking.open` provisions the role and returns the successful four-field
  result above with optional **`"refreshRequired": true`**. The field is omitted
  when the token already has the current list. When true, call
  **`auth.refresh {refresh_token}`** once (client: `SessionManager.refreshNow()`),
  securely replace **both** `access_token` and `refresh_token` from its result,
  then start ingest/serverTime with the **same tracking session** and new access
  token. No repeat open or tracking-generation restart is required. Both
  follow-up methods require the access token and enforce their normal scopes;
  sending points with the old token still fails with `-32003`.
  The pinned OAuth refresh implementation reloads user roles and role
  permissions from storage and preserves the login session ID.
- If refresh fails, do not start ingest: return to normal login, then open again.
  If the refreshed token still cannot ingest due to `-32003`, stop and surface
  the error rather than looping. Expired/invalid token errors stay `-32001`,
  compatible with the normal authorized RPC refresh/retry.
  Re-login also reloads permissions, but creates a new login/tracking session.
  Unregistered devices get `-32011` and are never provisioned.
- The reserved role's exact allowlist is checked at startup and on each open;
  conflicting, missing, or broadened role data fails closed (startup failure or
  internal error, not a refresh retry). Host admin CRUD excludes `system.*`
  roles. No operator permission grant is required for normal tracking.
- The pinned OAuth crate does not publicly expose the access token's `sid` or
  device claim, so the binding is subject + active device session of that
  subject; strict token↔device binding needs an OAuth crate change.
- Tests in `src/tracking/tests.rs` use in-memory storage ports with real SQLite
  OAuth registration/login/refresh and RBAC checks. Mongo document/allowlist
  helpers are tested, but the Mongo adapters have not been run against live MongoDB.

### Source layout

`src/main.rs` is the binary entry point; `src/app.rs` assembles the backend
without changing its runtime behavior. Authentication and administration live
under `src/auth/`, with user accounts, profiles, and roles split into
`src/auth/users/accounts.rs`, `profile.rs`, and `roles.rs`. Callback and mail
integrations are in `src/callback/` and `src/mail/`; `src/garage/` contains
garage RPC validation and Mongo storage; `src/tracking/` contains login
tracking wiring. Each domain keeps its tests in a
separate `tests.rs`.

### Authentication plugin

Both instances install the pinned [JSON-RPC-2-OAuth2](https://github.com/vsvictor/JSON-RPC-2-OAuth2)
plugin. MongoDB 8.0 stores users, profiles and RBAC in the persistent
`/var/lib/mongodb` data directory. It listens on loopback and the public IPv4 interface;
remote connections require TLS and authentication and are restricted by the
root-owned `/etc/brnavi/mongodb-allowed-cidr` allowlist (one IPv4 CIDR per line),
installed before MongoDB starts. The CA certificate is
`/etc/mongodb/tls/ca.crt`. Both instances use the same
`PASETO_LOCAL_KEY` and `AUTH_ISSUER`. The application authenticates as `brnavi`
with `readWrite` access only to `brnavi_auth`, not as the MongoDB administrator.
To open an authenticated interactive shell on the BRNavi host, run
`./scripts/mongo-shell.sh`; it prompts for the MongoDB administrator password.

Run `sh scripts/init-auth-env.sh` before the first native installation. It creates
`.env` if absent, restricts it to mode `0600`, and generates missing database,
broker, SRT and PASETO secrets. The installation stores runtime credentials in
root-protected system environment files. Keep those files and database backups
safe; do not regenerate credentials while reusing the databases. Rotating the
PASETO key invalidates existing tokens. Secrets are never committed.

MongoDB uses a separate volume from the earlier PostgreSQL configuration.
Existing PostgreSQL data is not deleted or automatically migrated.

The plugin exposes `auth.register`, `auth.login`, `auth.refresh`, `auth.getProfile`,
`auth.updateProfile`, and the permission-protected `rbac.*` methods.
`auth.getProfile` reads the authenticated user's current profile and roles
from MongoDB; unlike `navi.me`, it does not rely on the token's role snapshot.
`navi.me` uses `#[authorize]` and returns the verified user's ID, roles and
permissions. New users have no roles or administrative permissions.
The starter methods remain public; installing this plugin does not
automatically protect every RPC method or configure external OAuth providers.

The BRNavi-specific `admin.users.list`, `admin.users.create`,
`admin.users.update`, and `admin.users.delete` methods provide account
management for XRCenter. Listing requires `rbac.users.read`; create, update,
and delete require `rbac.users.write`. Password hashes are never returned.
Changing roles additionally requires `rbac.roles.write`; `admin.roles.list`
provides assignable roles, `admin.roles.create` creates a named role, and
`admin.roles.update` changes its permissions for administrators with
`rbac.roles.write`.
Updates can change email, profile fields, or password; deleting an account also
removes its linked external identities and an administrator cannot delete
their own account. Users created through the admin method use the same storage
format as `auth.register` and start without roles.

### Profile contacts and local backend validation

With the updated OAuth2 backend, `auth.register`, `auth.updateProfile`, and
`admin.users.create`/`admin.users.update` accept native contact arrays:
`"profile":{"contacts":[{"type":"email","value":"contact@example.invalid"}]}`.
Supported types: `phone`, `email`, `telegram`, `viber`, `facebook`, `x`,
`instagram`, `tiktok`, `whatsapp`, `signal`, `linkedin`, `website`.
Admin methods share the library's `ProfileContact` model
and validation; MongoDB stores BSON arrays. Lists replace in full; omitted
contacts remain unchanged; explicit `[]` stays empty. Older accounts with no
contacts return their login email as one email contact, including admin lists.
Login/reset email is independent; unverified profile contacts never authenticate
or link accounts. Values are trimmed, limited to 32 entries / 2048 bytes each,
and validated as email, phone/Viber/WhatsApp (7–15 digits), social handle or matching HTTPS
provider URL. Signal also accepts a `+` phone number and fragment-based `signal.me`
links; websites accept HTTP/HTTPS URLs without credentials. See the backend README for the full contract.

`Cargo.toml` pins each JSON-RPC plugin to a compatible upstream revision, and
`Cargo.lock` records the resolved dependency graph. MongoDB needs no schema
migration; legacy scalar custom `contacts` values must be converted to arrays
before rollout. Services are managed by systemd.

To bootstrap an administrator, first register an account, then use a trusted
MongoDB administrator session to grant the permissions and role. Replace the
email with the registered account's email:

```javascript
const auth = db.getSiblingDB("brnavi_auth");
db.roles.updateOne(
  { name: "user-manager" },
  { $set: { permissions: ["rbac.users.read", "rbac.users.write", "rbac.roles.write"] } },
  { upsert: true },
);
auth.users.updateOne(
  { email: "admin@example.test" },
  { $addToSet: { roles: "user-manager" } },
);
```

Sign in again after granting the role because access tokens contain a snapshot
of permissions. Keep this database bootstrap restricted to trusted operators.
To add another assignable role, create it in the same `roles` collection with
the permissions its users should receive:

```javascript
db.roles.updateOne(
  { name: "support-viewer" },
  { $set: { permissions: ["rbac.users.read"] } },
  { upsert: true },
);
```

Run this in `./scripts/mongo-shell.sh`, then click **Refresh list** in XRCenter. The
role appears in the edit dropdown for administrators with `rbac.roles.write`.

Example requests, sent through an encrypted transport (`register` requires `first_name`, `last_name`, `account_type` = `private`|`corporate`; `middle_name` may be empty):

```json
{"jsonrpc":"2.0","id":1,"method":"auth.register","params":{"email":"user@example.test","password":"<strong-password>","profile":{"first_name":"Ada","last_name":"Lovelace","middle_name":"","account_type":"private"}}}
```

```json
{"jsonrpc":"2.0","id":2,"method":"auth.login","params":{"email":"user@example.test","password":"<strong-password>","grant_type":"password"}}
```

```json
{"jsonrpc":"2.0","id":3,"method":"navi.me","params":{"access_token":"<access_token-from-login>"}}
```

Tokens work across both instances and all JSON-RPC transports, but passwords
and bearer tokens must not be sent over plaintext transports on untrusted
networks. Missing/invalid/expired access tokens return `-32001`; insufficient
RBAC permissions return `-32003`. Permissions retain the upstream token
snapshot semantics until expiration or refresh.

BRNavi logs every complete JSON-RPC request and response at `info` level across
all backend transports and batches. This intentionally includes submitted
passwords, access/refresh tokens, and profile fields. Restrict access to the
system journal and do not attach raw logs to public tickets.

### Native service management

The backend instances, gateway, MongoDB, Mosquitto and RabbitMQ run as native
systemd services. Build the backend with
`cargo build --locked --release --bin brnavi`; the gateway and backend units are
installed from `systemd/`. The `json-rpc-2-coopnav` dependency is fetched from
GitHub at the revision pinned in `Cargo.toml` and `Cargo.lock`; the build machine
must be able to access GitHub.

```text
brnavi/{Cargo.toml,Cargo.lock,src/}
```

Only the backend root `Cargo.lock` controls dependency resolution. Upload the
backend manifest, lockfile, and source before building; do not upload local
tracker checkouts, build artifacts, `.env`, or credentials. The current native
deployment has no Docker build context: Dockerfiles/Compose were removed. If
container builds are reintroduced, the build must be able to fetch the pinned
tracker revision from GitHub. Do not broaden a build context to include
credentials or unrelated projects.

Use `systemctl` and `journalctl` to inspect or
restart services. Do not expose the unauthenticated Control Plane or this
starter directly to the public internet without additional client
authentication and abuse protection.

```sh
sudo systemctl status brnavi brnavi-backend@1 brnavi-backend@2
sudo journalctl -u brnavi-gateway -u brnavi-backend@1 -f
```

The gateway accepts TCP and UDP on port `8443`. Configure the host and provider
firewalls before connecting clients. This starter does not enforce
authentication for all methods or provide production abuse protection.

Example JSON-RPC request (the response's `result` should alternate between the
two instance IDs as requests are balanced):

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"identify","params":{}}' | nc 127.0.0.1 8443
```

The gateway's Control Plane is restricted by the host firewall. Backend
instances listen on loopback-only ports and register with the gateway over
loopback; only the gateway ingress is exposed to clients.

### TLS and QUIC deployment

The public domain certificate belongs on the gateway, which terminates client
TLS and QUIC; it is not copied into the two backend instances. The RustRover
profile maps the leaf certificate, GoDaddy CA bundle, and matching private key
from `$USER_HOME$/Certs/mobilespace.app` to
`/home/victor/.ssh/brnavi-tls` on the server. These files are outside the
repository and excluded from Git.

After uploading renewed certificate files, restart the native services:

```sh
cd /home/victor/brnavi
sudo systemctl restart brnavi-gateway mosquitto rabbitmq-server
```

Systemd copies the certificate chain and private key to service-specific
runtime directories on each start; the key remains outside the repository and
is readable only by the corresponding service. The firewall blocks the
unauthenticated gateway Control Plane on TCP `8500` and allows client ingress
on TCP/UDP `8443`, MQTT TLS on TCP `8883`, AMQP TLS on TCP `5671`, and IP
protocols 33, 132 and 136. Backend instances reach the Control Plane at
`127.0.0.1:8500`.

Each native backend instance binds TCP/UDP on loopback ports `18081`/`18082`
and its internal TLS/QUIC listeners on `18443`/`18444`, respectively, so the
systemd instances do not conflict on shared host ports.

DCCP, SCTP and UDP-Lite are enabled on the shared listener, but external
firewalls must allow IP protocols 33, 132 and 136 (not TCP/UDP ports).
These transports require kernel support on both the server and client. MQTT and
AMQP broker endpoints use TLS and require the generated credentials; their
plaintext local ports are loopback-only. SRT and WebSocket/SRT clients also
need the generated `GATEWAY_SRT_PASSPHRASE` from `.env`. The server or provider firewall must
allow TCP and UDP `8443`, TCP `8883` and `5671`, and IP protocols 33, 132 and 136.
The host nftables rule cannot configure an external provider firewall. TLS
authenticates the server and broker endpoints,
not mobile clients; raw TCP/UDP and the other non-TLS transports are
unencrypted. The starter still lacks gateway client authentication, rate
limiting, and production abuse protection.

Client endpoint matrix (use `mobilespace.app` as the TLS server name):

| Transports | Endpoint |
|---|---|
| TCP, TLS, HTTP/1, WS-TCP, WS-TLS, WS-HTTP1 | TCP `8443` |
| UDP, RTP, RUDP, SRT, CoAP, QUIC, HTTP/3 and their WS variants | UDP `8443` |
| SCTP, WS-SCTP | IP protocol 132, port `8443` |
| DCCP, WS-DCCP | IP protocol 33, port `8443` |
| UDP-Lite, WS-UDP-Lite | IP protocol 136, port `8443` |
| MQTT, WS-MQTT | TLS broker TCP `8883`, user `brnavi`, `BRNAVI_BROKER_PASSWORD` |
| AMQP, WS-AMQP | TLS broker TCP `5671`, user `brnavi`, `BRNAVI_BROKER_PASSWORD`, vhost `/` |

The broker-backed transports do not accept connections on gateway port
`8443`. Their WS variants use upstream frame envelopes in broker messages,
not browser WebSocket connections to a broker. RabbitMQ explicitly permits
the transient non-exclusive request queues required by the pinned upstream
implementation. The gateway healthcheck queries `list_transports` on its
loopback Control Plane; systemd startup waits for all services to be ready
instead of reporting success when gateway initialization fails.

### Password-reset e-mail

Reset tokens are e-mailed to the account address (`src/mail/mod.rs`, SMTP STARTTLS).
Set `SMTP_HOST`, `SMTP_PORT` (587), `SMTP_USERNAME`, `SMTP_PASSWORD` and
`SMTP_FROM` in the ignored `.env` (see `.env.example`); never commit the password.
Without `SMTP_HOST` the token is pushed to the user's other signed-in device
(callback mechanism); `RESET_TOKEN_PUSH=1` keeps that push alongside e-mail.

### RustRover SFTP profile

The project includes a manual SFTP deployment profile under `.idea`. In
RustRover, open **Tools → Deployment → Browse Remote Host** to connect to
`95.216.88.141` as `victor`; use **Tools → Deployment → Upload to Mobilespace**
to upload the mapped project files to `/home/victor/brnavi`. The profile uses
`$USER_HOME$/.ssh/id_rsa` and does not store a password. The backend fetches
the pinned tracking dependency from GitHub during the build, so no local
ExServerTracker checkout is uploaded. RustRover may prompt for the key
passphrase. The profile intentionally does not auto-upload changes or include
`.env` secrets. After upload, install or update the native systemd units
separately on the server. The `https://mobilespace.app` value is only the
profile's web URL; it does not configure DNS, a TLS proxy, or HTTPS for the
gateway.

## Українська

Цей стартовий варіант запускає шлюз JSON-RPC-2 із вбудованим балансувальником
перед двома однаковими інстансами бекенду BRNavi. Шлюз і протокол бекенду
використовують upstream-проєкти:

- [JSON-RPC-2-GATEWAY](https://github.com/vsvictor/JSON-RPC-2-GATEWAY)
- [JSON-RPC-2](https://github.com/vsvictor/JSON-RPC-2)

Бінарний файл бекенду використовує upstream-крейт `json-rpc-2-backend`.
Кожен інстанс надає стартові методи крейту (`health`, `echo`, `identify` і
`rpc.list_methods`); `identify` повертає ID інстансу, щоб бачити розподіл
запитів між бекендами.

### Плагін автентифікації

Обидва інстанси встановлюють зафіксовану ревізію плагіна
[JSON-RPC-2-OAuth2](https://github.com/vsvictor/JSON-RPC-2-OAuth2).
MongoDB 8.0 зберігає користувачів, профілі й RBAC у постійному каталозі
`/var/lib/mongodb`. Вона слухає loopback і публічний IPv4-інтерфейс;
віддалені підключення вимагають TLS та автентифікацію й обмежені списком
`/etc/brnavi/mongodb-allowed-cidr` (доступний лише root; одна IPv4 CIDR на рядок),
який застосовується до запуску MongoDB. Сертифікат CA:
`/etc/mongodb/tls/ca.crt`. Інстанси використовують спільні
`PASETO_LOCAL_KEY` та `AUTH_ISSUER`. Застосунок підключається як `brnavi`
із правами `readWrite` лише для `brnavi_auth`, не як адміністратор MongoDB.
Щоб відкрити інтерактивну автентифіковану консоль на сервері BRNavi,
виконайте `./scripts/mongo-shell.sh`; скрипт запитує пароль адміністратора
MongoDB.

Перед першим нативним встановленням виконайте `sh scripts/init-auth-env.sh`.
Скрипт створює `.env`, якщо його немає, встановлює права `0600` та генерує
відсутні паролі БД і брокера, SRT passphrase та PASETO v4.local ключ.
Інсталяція зберігає runtime-секрети у системних env-файлах, доступних root.
Захищайте їх і резервні копії БД; не генеруйте нові паролі для наявних баз.
Зміна PASETO-ключа робить наявні токени невалідними. Секрети не потрапляють у Git.

MongoDB відокремлена від попередньої конфігурації PostgreSQL.
Наявні дані PostgreSQL не видаляються й не переносяться автоматично.

Плагін надає `auth.register`, `auth.login`, `auth.refresh`, `auth.getProfile`,
`auth.updateProfile` та захищені дозволами методи `rbac.*`.
`auth.getProfile` читає актуальний профіль і ролі автентифікованого користувача
з MongoDB, а не snapshot ролей із токена, як `navi.me`.
Метод `navi.me` використовує `#[authorize]` і повертає перевірені ID
користувача, ролі й дозволи. Нові користувачі не мають ролей чи
адміністративних дозволів. Стартові методи залишаються публічними:
підключення плагіна не захищає кожний RPC автоматично й не налаштовує
зовнішніх OAuth-провайдерів.

Методи BRNavi `admin.users.list`, `admin.users.create`, `admin.users.update`
та `admin.users.delete` надають XRCenter керування акаунтами. Для перегляду
потрібен `rbac.users.read`, для створення, зміни й видалення —
`rbac.users.write`. Для керування ролями додатково потрібен
`rbac.roles.write`: метод `admin.roles.list` повертає доступні ролі, а
`admin.roles.create` додає роль із вказаними дозволами, `admin.roles.update`
змінює її дозволи.
Хеші паролів не повертаються. Оновлення змінює email, поля профілю, пароль або
ролі; видалення також прибирає пов’язані зовнішні identities, а адміністратор
не може видалити власний акаунт. Створені через admin-метод користувачі
зберігаються у форматі `auth.register` і не отримують ролей.

### Плагін CarAPI

Під час запуску бекенда BRNavi встановлює зафіксований плагін
[`json-rpc-2-car`](https://github.com/vsvictor/JSON-RTC-2-Car/tree/main/crates/json-rpc-2-car).
`BackendBuilder::from_dotenv()` завантажує локальні значення `.env`, а
`CarConfig::from_env()` читає `CARAPI_TOKEN` і `CARAPI_SECRET` та передає їх
плагіну. Для systemd ті самі змінні задаються у `/etc/brnavi/backend.env`.
Не зберігайте облікові дані у файлах під контролем Git; `.env.example` містить
лише порожні значення.

### Трекінг після входу (CoopNav)

Бекенд встановлює `CoopNavPlugin` з [ExServerTracker](../ExServerTracker)
та додає хост-метод, що пов'язує пристрій входу з сесією трекінгу. Етап 2
вимкнений, доки на обох юнітах не задано `COOPNAV_STAGE2=true` з однаковими
`COOPNAV_EPOCH`, `COOPNAV_COOP_BRAND` і `COOPNAV_REDIS_URL`. Етап 3 лишається
вимкненим. Новий ID не створюється: `deviceId`, який клієнт уже надсилає в
`auth.login`/`auth.loginWithGoogle`, стає ID трекера.

`tracking.open` — параметри `{access_token | authorization: "Bearer …", deviceId}`
(невідомі поля відхиляються; `deviceId` — 1–128 символів `[A-Za-z0-9._:-]`).
Початкова авторизація — `auth.authorize(params, &[])`. Сервер вимагає, щоб перевірений
суб'єкт мав чинну OAuth-сесію пристрою (рядок `sessions` з тими ж `user_id` і
`device_id` та свіжим `last_seen`). Потім ідемпотентно додає цьому власнику
фіксовану роль `system.xrnavi.tracking`, також для наявних користувачів, і
штатним API OAuth перевіряє обидва дозволи трекінгу. Результат:

```json
{"trackerId": "<deviceId>", "sessionId": "<32 hex>", "driverId": "<subject>", "brandId": "personal:<subject>"}
```

`brandId` — особистий простір трекінгу, а не автопарк: користувач пише лише в
`personal:<власний subject>`. `sessionId` — 128 випадкових біт, стабільний для
тієї ж пари (суб'єкт, пристрій, OAuth-вхід) і замінюється після нового входу.
Помилки: `-32011` пристрій не зареєстрований для користувача / сесія недійсна,
`-32602` некоректні параметри. Далі клієнт викликає
`tracking.ingest {sessionId, driverId, brandId, points[1..100]}` і
`serverTime {sessionId}`; кожен виклик перевіряє, що OAuth-сесія пристрою та
сама й неактивна не довше 30 днів.

Сховище (база автентифікації): `tracking_sessions` (`_id: {subject, device_id,
auth_sid}`, унікальний `session_id`, індекс створюється під час запуску) і
`tracker_points` (`_id: {brand_id, driver_id, point_id}`; повтор однакової
точки ідемпотентний, інший вміст з тим самим ID — конфлікт). Поки
`COOPNAV_STAGE2` дорівнює false, Redis не відкривається. За `true` обидва
юніти ділять один локальний індекс донорів у Redis (`coopnav:v1`); історія
Mongo лишається в `personal:<subject>`.

Автоматичні дозволи й повтор клієнта:
- Під час запуску роль `system.xrnavi.tracking` у наявній OAuth-колекції
  `roles` записується рівно як `tracking.ingest` і `tracking.server_time`,
  а за `COOPNAV_STAGE2=true` ще й `coop.sync_peers`. `tracking.open` за ObjectId
  перевіреного власника виконує `$addToSet` цієї ролі в `users.roles`,
  зберігаючи інші ролі. Немає довільного user/role від клієнта, `*` чи обходу
  перевірок. Нові й наявні користувачі отримують роль під час першого коректного open.
- Виданий access token незмінний. Якщо бракує дозволу з поточного списку,
  `tracking.open` призначає роль і повертає успішний результат із чотирма
  полями вище та optional **`"refreshRequired": true`**. Якщо токен уже має
  поточний список, це поле відсутнє. Коли true, один раз викличте
  **`auth.refresh {refresh_token}`** (клієнт: `SessionManager.refreshNow()`),
  безпечно замініть **обидва** токени з результату й запускайте
  ingest/serverTime з **тією самою tracking-сесією** та новим access token.
  Повторний open чи перезапуск tracking generation не потрібен.
  Обидва методи вимагають access token і штатно перевіряють дозволи;
  ingest зі старим токеном усе ще повертає `-32003`.
  Штатний refresh перечитує ролі й дозволи з бази та зберігає ID сесії входу.
- Якщо refresh не вдався — не запускайте ingest; звичайний повторний login,
  потім open. Якщо після refresh ingest повертає `-32003` — зупиніться й
  покажіть помилку, без циклу. Помилки простроченого/некоректного токена
  залишаються `-32001`, сумісно зі штатним refresh/retry авторизованих RPC.
  Повторний login також завантажує дозволи, але створює нову сесію входу/трекінгу.
  Незареєстрований пристрій отримує `-32011`, роль йому не призначається.
- Точний список дозволів зарезервованої ролі перевіряється під час запуску
  й кожного open; конфліктні, відсутні чи розширені дозволи блокують операцію
  (помилка запуску або внутрішня помилка, не повтор refresh).
  Адміністративний CRUD хоста виключає `system.*` ролі.
  Ручне призначення дозволів для звичайного трекінгу більше не потрібне.
- Закріплена OAuth-бібліотека не відкриває публічно `sid` чи claim пристрою в
  токені, тому прив'язка — це суб'єкт + активна сесія пристрою цього суб'єкта;
  строга прив'язка токена до пристрою потребує зміни OAuth-бібліотеки.
- Тести в `src/tracking/tests.rs` використовують порти сховища в пам'яті та
  справжні SQLite OAuth registration/login/refresh і RBAC. Перевірено Mongo
  BSON/allowlist helpers, але Mongo-адаптери не запускали на живій MongoDB.

### Структура коду

`src/main.rs` — точка входу бінарного файлу; `src/app.rs` збирає бекенд без
зміни його поведінки під час запуску. Автентифікація й адміністрування
розташовані в `src/auth/`; керування акаунтами, профілями й ролями розділено
між `src/auth/users/accounts.rs`, `profile.rs` та `roles.rs`. Інтеграції
callback і пошти містяться в `src/callback/` та `src/mail/`; `src/tracking/` —
трекінг після входу; тести кожного
домену залишаються в окремому `tests.rs`.

### Контакти профілю й локальна перевірка backend

З оновленою OAuth2-бібліотекою `auth.register`, `auth.updateProfile`,
`admin.users.create` та `admin.users.update` приймають справжній масив:
`"profile":{"contacts":[{"type":"email","value":"contact@example.invalid"}]}`.
Типи: `phone`, `email`, `telegram`, `viber`, `facebook`, `x`, `instagram`, `tiktok`,
`whatsapp`, `signal`, `linkedin`, `website`.
Admin-методи використовують спільну модель `ProfileContact` і валідацію
бібліотеки; MongoDB зберігає BSON-масиви. Список замінюється цілком; відсутнє
поле не змінює контакти; явне `[]` лишається порожнім. Старі акаунти без поля
отримують один контакт із login email, зокрема у списку адміністратора.
Email входу/скидання незалежний: неперевірені контакти не дозволяють вхід і не
зв'язують акаунти. Значення обрізаються по краях, максимум 32 контакти по
2048 байтів; перевіряється email, телефон/Viber/WhatsApp (7–15 цифр), handle соцмережі
або HTTPS URL відповідного провайдера. Signal також приймає номер із `+` і
посилання `signal.me` із фрагментом; вебсайти — HTTP/HTTPS URL без credentials.
Повний контракт — у README бібліотеки.

`Cargo.toml` закріплює кожен JSON-RPC-плагін на сумісній upstream-ревізії, а
`Cargo.lock` фіксує отриманий граф залежностей. MongoDB не потребує міграції
схеми; старі власні скалярні `contacts` перед запуском слід перетворити на
масиви. Сервіси запускаються й перезапускаються через systemd.

Щоб створити адміністратора, спочатку зареєструйте акаунт, а потім надайте
йому роль і дозволи через довірену адміністративну сесію MongoDB. Замініть
email на адресу зареєстрованого акаунта:

```javascript
const auth = db.getSiblingDB("brnavi_auth");
auth.roles.updateOne(
  { name: "user-manager" },
  { $set: { permissions: ["rbac.users.read", "rbac.users.write", "rbac.roles.write"] } },
  { upsert: true },
);
auth.users.updateOne(
  { email: "admin@example.test" },
  { $addToSet: { roles: "user-manager" } },
);
```

Після надання ролі увійдіть знову, оскільки access token містить snapshot
дозволів. Доступ до цього bootstrap слід залишити лише довіреним операторам.
Щоб додати іншу роль до списку XRCenter, створіть документ у колекції `roles`
із потрібними їй дозволами:

```javascript
auth.roles.updateOne(
  { name: "support-viewer" },
  { $set: { permissions: ["rbac.users.read"] } },
  { upsert: true },
);
```

Виконайте команду у консолі `./scripts/mongo-shell.sh`, потім натисніть
**«Оновити список»** у XRCenter. Роль з’явиться у випадаючому списку
адміністраторів із дозволом `rbac.roles.write`.

Приклади запитів через зашифрований транспорт (`register` потребує `first_name`, `last_name`, `account_type` = `private`|`corporate`; `middle_name` може бути порожнім):

```json
{"jsonrpc":"2.0","id":1,"method":"auth.register","params":{"email":"user@example.test","password":"<strong-password>","profile":{"first_name":"Ada","last_name":"Lovelace","middle_name":"","account_type":"private"}}}
```

```json
{"jsonrpc":"2.0","id":2,"method":"auth.login","params":{"email":"user@example.test","password":"<strong-password>","grant_type":"password"}}
```

```json
{"jsonrpc":"2.0","id":3,"method":"navi.me","params":{"access_token":"<access_token-from-login>"}}
```

Токени працюють на обох інстансах і всіх JSON-RPC-транспортах, але паролі
та bearer-токени не можна передавати відкритим текстом у недовіреній мережі.
Відсутні, невалідні чи прострочені access-токени повертають `-32001`;
недостатні RBAC-дозволи — `-32003`. Дозволи зберігають snapshot-семантику
upstream до завершення строку дії токена чи його поновлення.

BRNavi журналює повний вміст кожного JSON-RPC-запиту й відповіді на рівні
`info` для всіх транспортів бекенду та batch-запитів. Це включає передані
паролі, access/refresh-токени й поля профілю. Обмежте доступ до systemd journal;
не додавайте такі журнали до публічних звернень.

### Керування нативними службами

Інстанси backend, gateway, MongoDB, Mosquitto та RabbitMQ працюють як нативні
служби systemd. Зберіть backend командою
`cargo build --locked --release --bin brnavi`; unit-файли gateway і backend
розміщені в `systemd/`. Залежність `json-rpc-2-coopnav` завантажується з
GitHub за ревізією, зафіксованою в `Cargo.toml` і `Cargo.lock`; машина збірки
має мати доступ до GitHub.

```text
brnavi/{Cargo.toml,Cargo.lock,src/}
```

Залежності фіксує кореневий `Cargo.lock` backend. Перед збіркою завантажте
manifest, lockfile та вихідний код backend; не передавайте локальні копії
трекера, артефакти збірки, `.env` чи облікові дані. Поточний native deployment
не має Docker build context: Dockerfiles/Compose видалені. Якщо контейнерну
збірку повернуть, вона має мати змогу завантажити зафіксовану ревізію трекера
з GitHub. Не включайте секрети чи сторонні проєкти, розширюючи build context.

Для перегляду стану й перезапуску використовуйте
`systemctl` та `journalctl`. Не відкривайте неавтентифікований Control Plane
або цей starter до публічного інтернету без додаткової автентифікації та
захисту від зловживань.

```sh
sudo systemctl status brnavi brnavi-backend@1 brnavi-backend@2
sudo journalctl -u brnavi-gateway -u brnavi-backend@1 -f
```

Шлюз приймає TCP та UDP на порту `8443`. Перед підключенням клієнтів
налаштуйте host і provider firewalls. Не всі методи вимагають автентифікації,
а starter не має production-захисту від зловживань.

Приклад JSON-RPC-запиту (значення `result` має змінюватися між ID двох
інстансів у міру балансування запитів):

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"identify","params":{}}' | nc 127.0.0.1 8443
```

Control Plane шлюзу обмежений host firewall. Інстанси backend слухають лише
loopback-порти й реєструються на gateway через loopback; клієнтам доступний
лише ingress шлюзу.

### Деплой TLS та QUIC

Сертифікат публічного домену встановлюється на шлюзі, який завершує TLS- та
QUIC-з'єднання мобільних клієнтів; до двох інстансів бекенду він не
копіюється. Профіль RustRover зіставляє сертифікат домену, CA bundle GoDaddy
та відповідний приватний ключ із `$USER_HOME$/Certs/mobilespace.app` у
`/home/victor/.ssh/brnavi-tls` на сервері. Файли зберігаються поза
репозиторієм і виключені з Git.

Після завантаження оновлених сертифікатів перезапустіть нативні служби:

```sh
cd /home/victor/brnavi
sudo systemctl restart brnavi-gateway mosquitto rabbitmq-server
```

Systemd копіює сертифікатний ланцюжок і приватний ключ у окремі тимчасові
каталоги служб під час кожного запуску. Ключ не зберігається в репозиторії й
доступний лише відповідній службі. Firewall блокує неавтентифікований Control
Plane на TCP `8500` і дозволяє ingress клієнтів TCP/UDP `8443`, MQTT TLS TCP
`8883`, AMQP TLS TCP `5671` та IP-протоколи 33, 132 і 136. Backend-інстанси
під'єднуються до Control Plane за адресою `127.0.0.1:8500`.

Кожен нативний backend слухає TCP/UDP на loopback-портах `18081`/`18082`, а
внутрішні TLS/QUIC-listener-и — на `18443`/`18444` відповідно, щоб інстанси
systemd не конфліктували за спільні порти хоста.

DCCP, SCTP і UDP-Lite увімкнені на спільному listener-і, але зовнішній firewall
має дозволяти IP-протоколи 33, 132 і 136 (це не TCP/UDP-порти). Ці транспорти
потребують підтримки ядром і сервера, і клієнта. Брокери MQTT та AMQP
працюють через TLS і потребують згенерованих облікових даних; локальні
незашифровані порти доступні лише через loopback. Клієнтам SRT і
WebSocket/SRT також потрібен згенерований `GATEWAY_SRT_PASSPHRASE` із `.env`.
Firewall сервера або
провайдера має дозволяти TCP і UDP `8443`, TCP `8883` і `5671`, а також
IP-протоколи 33, 132 і 136. Правило nftables на сервері не налаштовує зовнішній firewall провайдера. TLS автентифікує сервер і брокерні
endpoint-и, але не мобільних клієнтів; TCP/UDP та інші не-TLS-транспорти не
шифруються. Starter не має автентифікації клієнтів шлюзу, обмеження частоти
запитів чи production-захисту.

Матриця клієнтських endpoint-ів (TLS-ім'я сервера — `mobilespace.app`):

| Транспорти | Endpoint |
|---|---|
| TCP, TLS, HTTP/1, WS-TCP, WS-TLS, WS-HTTP1 | TCP `8443` |
| UDP, RTP, RUDP, SRT, CoAP, QUIC, HTTP/3 та їхні WS-варіанти | UDP `8443` |
| SCTP, WS-SCTP | IP-протокол 132, порт `8443` |
| DCCP, WS-DCCP | IP-протокол 33, порт `8443` |
| UDP-Lite, WS-UDP-Lite | IP-протокол 136, порт `8443` |
| MQTT, WS-MQTT | TLS-брокер TCP `8883`, користувач `brnavi`, `BRNAVI_BROKER_PASSWORD` |
| AMQP, WS-AMQP | TLS-брокер TCP `5671`, користувач `brnavi`, `BRNAVI_BROKER_PASSWORD`, vhost `/` |

Брокерні транспорти не приймають з'єднань на порту шлюзу `8443`. Їхні
WS-варіанти передають upstream frame envelope у повідомленнях брокера, а не
через браузерний WebSocket до брокера. RabbitMQ явно дозволяє тимчасові
неексклюзивні черги запитів, потрібні зафіксованій upstream-реалізації.
Healthcheck шлюзу запитує `list_transports` через loopback Control Plane;
systemd очікує готовності всіх сервісів замість повідомлення про успішний
старт, якщо ініціалізація шлюзу завершується помилкою.

### Лист відновлення пароля

Токен відновлення надсилається листом на адресу акаунта (`src/mail/mod.rs`, SMTP STARTTLS).
Задайте `SMTP_HOST`, `SMTP_PORT` (587), `SMTP_USERNAME`, `SMTP_PASSWORD` та
`SMTP_FROM` у виключеному з Git `.env` (див. `.env.example`); пароль не комітьте.
Без `SMTP_HOST` токен надходить push-ом на інший авторизований пристрій користувача
(механізм callback); `RESET_TOKEN_PUSH=1` зберігає цей push разом із листом.

### Профіль SFTP у RustRover

У `.idea` додано профіль ручного SFTP-деплою. У RustRover відкрийте
**Tools → Deployment → Browse Remote Host**, щоб під'єднатися до
`95.216.88.141` як `victor`; **Tools → Deployment → Upload to Mobilespace**
завантажує зіставлені файли проєкту до `/home/victor/brnavi`. Профіль
використовує `$USER_HOME$/.ssh/id_rsa` і не зберігає пароль. Під час збірки
backend завантажує зафіксовану залежність трекера з GitHub, тому локальну копію
ExServerTracker не потрібно передавати. RustRover може запросити парольну фразу
ключа. Автоматичне завантаження вимкнене, `.env` із секретами не включено.
Після передачі файлів нативні systemd-служби потрібно окремо встановити або
оновити на сервері. Значення `https://mobilespace.app` використане лише як
веб-URL профілю: воно не налаштовує DNS, TLS-проксі чи HTTPS для шлюзу.
