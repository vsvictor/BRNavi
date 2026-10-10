| `startShift` | all required fields present | success (`result`) | unit + e2e | `quic-jsonrpc-server/src/lib.rs::start_shift_accepts_payload_and_returns_result`, `quic-jsonrpc-client/tests/e2e.rs::start_and_finish_shift_forward_contract_to_upstream` |
| `startShift` | missing `accessToken` | `error.code = -32602` | e2e | `quic-jsonrpc-client/tests/e2e.rs::start_shift_without_access_token_returns_invalid_params` |
| `startShift` | missing `deviceTime` | `error.code = -32602` | e2e | `quic-jsonrpc-client/tests/e2e.rs::start_shift_without_device_time_returns_invalid_params` |
| `startShift` | missing `sendTime` | `error.code = -32602` | e2e | `quic-jsonrpc-client/tests/e2e.rs::start_shift_without_send_time_returns_invalid_params` |
| `startShift` | missing `vehicleId` | `error.code = -32602` | unit + e2e | `quic-jsonrpc-server/src/lib.rs::start_shift_rejects_missing_required_fields`, `quic-jsonrpc-client/tests/e2e.rs::start_shift_without_vehicle_id_returns_invalid_params` |
| `finishShift` | all required fields present | success (`result`) | unit + e2e | `quic-jsonrpc-server/src/lib.rs::finish_shift_accepts_payload_and_returns_result`, `quic-jsonrpc-client/tests/e2e.rs::start_and_finish_shift_forward_contract_to_upstream` |
| `finishShift` | missing `accessToken` | `error.code = -32602` | unit + e2e | `quic-jsonrpc-server/src/lib.rs::finish_shift_rejects_missing_access_token`, `quic-jsonrpc-client/tests/e2e.rs::finish_shift_without_access_token_returns_invalid_params` |
| `finishShift` | missing `deviceTime` | `error.code = -32602` | unit + e2e | `quic-jsonrpc-server/src/lib.rs::finish_shift_rejects_missing_device_time`, `quic-jsonrpc-client/tests/e2e.rs::finish_shift_without_device_time_returns_invalid_params` |
### How to run only shift contract tests

Run only shift-related unit tests from server crate:

```bash
cargo test -p quic-jsonrpc-server start_shift
cargo test -p quic-jsonrpc-server finish_shift
```

Run only shift-related e2e tests from client crate:

```bash
cargo test -p quic-jsonrpc-client --test e2e start_and_finish_shift_forward_contract_to_upstream
cargo test -p quic-jsonrpc-client --test e2e start_shift_without_
cargo test -p quic-jsonrpc-client --test e2e finish_shift_without_
```

CI-friendly variants (single test thread + logs):

```bash
cargo test -p quic-jsonrpc-server start_shift -- --test-threads=1 --nocapture
cargo test -p quic-jsonrpc-server finish_shift -- --test-threads=1 --nocapture
cargo test -p quic-jsonrpc-client --test e2e start_and_finish_shift_forward_contract_to_upstream -- --test-threads=1 --nocapture
cargo test -p quic-jsonrpc-client --test e2e start_shift_without_ -- --test-threads=1 --nocapture
cargo test -p quic-jsonrpc-client --test e2e finish_shift_without_ -- --test-threads=1 --nocapture
```
## Email login contract

The mobile client sends JSON-RPC method `login` with exactly two parameters:

```json
{"jsonrpc":"2.0","method":"login","id":"1","params":{"login":"driver@example.com","password":"5f4dcc3b5aa765d61d8327deb882cf99"}}
```

`password` is the 32-character hexadecimal MD5 of the UTF-8 password, retained for compatibility with the mobile wire contract. It is a **password-equivalent secret**: possession is sufficient to log in. Send it only over the authenticated TLS/QUIC connection, never in URLs, command arguments, logs, or environment variables. Uppercase hex is accepted and normalized to lowercase. MD5 is not used as a password-storage algorithm.

Production login verifies the account in MongoDB; it **does not make an HTTP login request**. `login` is an ASCII email address, consistently trimmed and lowercased for lookup, session identity and account-level rate limiting. The server stores only a randomly salted **Argon2id PHC hash of the normalized MD5**, not plaintext passwords or raw MD5. Expensive verification runs on a bounded blocking-worker pool, not Tokio workers. An empty collection rejects every login; there are no demo credentials, automatic signup, or fallback HTTP authentication. Legacy phone/device login is no longer accepted by production authentication (legacy test adapters remain explicitly injectable).

Successful login returns `result.status = "ok"` and a registered `result.session.sessionId`, with `user.id`, `user.login`, `session.tokenType`, `session.accessToken` and `session.expiresInSec`. No local session is registered unless credentials were verified and the authenticator returned `status: "ok"`. A session-write failure fails login rather than reporting unusable success.

| Failure | JSON-RPC code/message |
|---|---|
| Unknown, disabled, external-only account, or incorrect password equivalent | `-32005`, `Invalid credentials` (identical for all cases) |
| Authentication database unavailable/corrupt credential | `-32006`, `Authentication unavailable` (sanitized) |
| Session persistence failure | `-32000`, `Internal session storage error` |
| Invalid email/MD5 input | `-32602`, `Invalid params` |
| Rate limit exceeded | `-32004`, existing retry message |

Email logins without a device identifier have account-scoped resumable sessions (`device_id = "email-login"`). Login rate limiting applies across IP addresses to the `login` identifier. Payload logs mask `login` and redact the password hash, tokens, and session IDs.

The HTTP gateway is still used by `getShift`, `startShift`, and `finishShift`; their existing upstream bearer-token contract is unchanged. A `local-...` access token from Mongo login is a local session credential, **not** a 1C/driver-API token or an OAuth access token. It cannot grant access to the independent upstream shift API; that integration needs a separately provisioned upstream credential/token exchange in a future change.

## Account registration contract

The mobile client sends `createAccount` over the existing TLS/QUIC JSON-RPC transport (no HTTP):

```json
{"jsonrpc":"2.0","method":"createAccount","id":"2","params":{"name":"Test Driver","email":"driver@example.com","password":"5f4dcc3b5aa765d61d8327deb882cf99"}}
```

Exactly these three string fields are accepted. The full name is trimmed, must contain at least
two whitespace-separated words, be 2–100 Unicode characters long, and contain no control
characters. Email uses the same ASCII validation, trimming and lowercasing as login. Registration
requires **exactly 32 lowercase hexadecimal MD5 characters** (unlike legacy login, which also
accepts uppercase). The password equivalent receives a salted Argon2id hash; neither plaintext
nor raw MD5 is stored. Name is stored only in the collection named **`profile`**, never `users`.

Only after the user and linked profile have been persisted and the user enabled, the server returns:

```json
{"jsonrpc":"2.0","id":"2","result":{"status":"ok","created":true}}
```

Registration does **not** log in, create a session, mint access tokens, or call the upstream API.
The client should show confirmation and navigate to sign-in only on this result. JSON-RPC
notifications do not receive confirmation; clients must include an `id`. Registration shares the
existing IP and normalized-email credential rate-limit buckets with login.

| Registration failure | JSON-RPC code/message |
|---|---|
| Invalid/missing/extra fields, invalid full name/email/lowercase MD5 | `-32602`, `Invalid params` |
| Normalized email already registered (including disabled/provisioning users) | `-32007`, `Email already registered` |
| Hash/storage/provisioning failure, compensation completed | `-32008`, `Account creation unavailable` |
| Compensating cleanup failed, or provisioning worker failed | `-32009`, `Account creation failed; cleanup incomplete` |
| Rate limit exceeded | `-32004`, existing retry message |

### Standalone MongoDB provisioning limitations

This implementation works with standalone MongoDB; it does **not** claim a cross-collection
atomic transaction. It inserts a **disabled** user, inserts its profile with a unique ObjectId
`user_id`, then enables that user. Disabled staged users cannot authenticate. Failures delete
only this attempt's newly generated user/profile IDs; no existing account is overwritten or
deleted, including concurrent duplicate email attempts. Cleanup removes the user before the
profile, so a failed user deletion cannot leave an enabled user without its profile.
An insert/enable acknowledgement failure is treated as failure and compensated, not success.
Provisioning continues in an owned Tokio task if the requesting stream is cancelled.

Process termination, ambiguous network outcomes, or unavailable cleanup can leave staged
records (and reserve the email) requiring operator inspection/reconciliation; no automatic
deletion of existing disabled accounts is performed. A lost success reply can also mean the
account exists although the client did not receive confirmation; a retry returns duplicate.
Concurrent administration must not enable/delete staged users or edit their profiles.
Genuine atomic registration requires a supported replica-set/sharded transaction deployment
and a separate transaction implementation; this change does not reconfigure MongoDB.

### MongoDB users/profile schema and startup

Every normal server startup (even with `QUIC_RPC_SESSION_BACKEND=memory`) requires the existing `QUIC_RPC_MONGO_*` connection settings and prepares the `users` collection in `QUIC_RPC_MONGO_DB`. Startup creates it if missing, applies a strict/error JSON schema validator with `collMod`, and creates these unique indexes:

* `users_login_unique`: `login_normalized` (trimmed lowercase email).
* `profile_user_unique`: unique `profile.user_id` ObjectId. Startup also creates/configures
  the exact `profile` collection with a strict validator. A profile contains ObjectId `_id`,
  ObjectId `user_id`, `name`, and BSON dates `created_at`/`updated_at`.
* `users_oidc_identity_unique`: compound `identities.issuer` + `identities.subject`, a unique multikey index with a partial filter excluding missing/empty identity arrays. Empty identity lists are allowed on many local accounts. Identity objects have exactly the required `issuer`/`subject` fields; duplicate identity objects within one account are rejected by the array validator.

Collection/schema/index setup errors fail startup; they never weaken validation or enable alternative login. The MongoDB service/admin identity needs permissions to create collections/indexes and run `collMod`, as well as the existing read/write/session permissions. Existing duplicate logins/identities make unique index setup fail and must be corrected administratively. Applying `collMod` does not retroactively validate old documents; it strictly validates subsequent writes. Existing invalid documents are not silently overwritten, and malformed stored credentials fail authentication.

Documents contain an ObjectId `_id`, `login_normalized`, `enabled` (boolean), optional `password_hash` (Argon2id PHC string), `identities` (array), and BSON dates `created_at`/`updated_at`. Newly provisioned local users are enabled with an empty identity array. To disable future password logins, administratively set `enabled: false`; existing resumable sessions must be separately revoked/removed if immediate logout is required.

The identity schema is **preparation for future OAuth/OIDC**, not an implemented OAuth2 flow. An external identity is the exact, case-sensitive trusted **issuer + subject** pair, not a provider label or an email address. There is no OAuth authorization, code/token exchange, ID-token verification, automatic account linking, or linking by matching email. Password login remains local Mongo authentication. Future identity attachment must be an explicitly authorized operation; users without `password_hash` cannot use local password login.

### Secure administrative provisioning

From the workspace root:

```bash
cargo run -p quic-jsonrpc-server --bin create-user -- driver@example.com
```

Or after `cargo build -p quic-jsonrpc-server --bins`:

```bash
./target/debug/create-user driver@example.com
```

The utility loads the same service/deployed `.env` with process environment taking precedence, before starting any threads. Environment parse failures are fatal and never print a credential-bearing line. It prompts for the **32-hex MD5 password equivalent with terminal echo disabled**. No password/MD5 argument or environment variable is supported. A trusted secret manager can also provide a single MD5 line via stdin (do not use a shell literal, `echo`, or command-line arguments containing the secret). The CLI normalizes/validates the email and MD5, applies the same schema/indexes, hashes with fresh Argon2id salt, rejects duplicate logins without overwriting users, and prints only `User created.` on success. No production account is created until an administrator explicitly invokes it.

The default `cargo run -p quic-jsonrpc-server` still starts the server; provisioning is a separate binary.
The administrative `create-user` utility retains its existing enabled-user-only behavior and
does not create a profile; use `createAccount` for full registration with a name/profile.

### Authentication validation

The registration integration test uses local MongoDB at `127.0.0.1:27017`, without reading
service credentials, and creates/drops an isolated uniquely named database with exact `users`
and `profile` collections. It checks successful linked profile creation, duplicate/no-overwrite
behavior, concurrent registration, ObjectId/unique profile validation, profile/enable failures,
owned-record compensation and explicit cleanup failure. Unit tests cover invalid registration
inputs, normalized-email/IP rate limiting, and registration without a login session.

```bash
cargo test -p quic-jsonrpc-server --lib
cargo test -p quic-jsonrpc-server --lib mongo_users_authentication_and_schema -- --ignored
cargo test -p quic-jsonrpc-server --lib mongo_create_account_staged_provisioning -- --ignored
```

The opt-in Mongo test reads the service `.env` without mutating the test process environment, creates a uniquely named test collection in the configured database, checks empty/missing/disabled/wrong credentials, normalized duplicate logins, hash/schema enforcement and issuer+subject uniqueness, then drops that collection. It never provisions production users. Existing client transport/upstream e2e fixtures explicitly inject their legacy test authenticator, so they do not require MongoDB or change the production authentication source.
## Vehicle catalog and private garage

The production composition includes the Mongo-backed catalog/garage service.
Catalog import is explicit, never part of normal startup; see
[`../data/vehicle-catalog/v1.0.2/README.md`](../data/vehicle-catalog/v1.0.2/README.md)
for provenance, full database license, public export schema and deployment obligations.

Public RPCs (all pages are `{items,nextCursor}`):

- `catalog.makes {query?,cursor?,limit?}` → `{id,name}` items.
- `catalog.models {makeId,query?,cursor?,limit?}` → `{id,name}` items.
- `catalog.generations {modelId,cursor?,limit?}` → `{id,name,yearStart,yearEnd}` items.
- `catalog.variants {generationId,cursor?,limit?}` → `{id,make,model,generation,engine,yearStart,yearEnd,lengthMm,widthMm,heightMm,curbWeightKg}` items.
- `catalog.license {}` → license/provenance/public retrieval instructions.
- `catalog.export {kind?,cursor?,limit?}` → complete public licensed database pages;
  see linked schema. No session is required or accepted in catalog requests.

Private RPCs:

- `garage.list {sessionId}` → `{vehicles,selectedVehicleId}`.
- `garage.add {sessionId,catalogVariantId,year,dimensions:{height,width,length,mass},clientRequestId}`
  → `{vehicle,selectedVehicleId}`. `clientRequestId` is a **required** stable UUID
  minted once per submission and reused on native transport/application retry.
  Exact retries return the same vehicle (and the current persisted selection);
  key reuse with different variant/year/dimensions is `-32602`.
- `garage.select {sessionId,vehicleId}` → `{selectedVehicleId}`.

A garage vehicle is `{id,catalogVariantId,make,model,generation,engine,year,dimensions}`.
Garage dimensions are **metres and tonnes**, not catalog mm/kg; actual loaded
mass is entered by the user, with curb weight only a catalog prefill suggestion.
Finite positive limits: height/width ≤10 m, length ≤50 m, mass ≤200 t.
Year is 1900 through current UTC year +1, intersected with known generation bounds.
Missing bounds/dimensions are never invented. Page limits default 50, maximum 100;
invalid inputs return `-32602`, unavailable catalog `-32020`, garage storage/capacity
failure `-32021`, unknown or another owner's vehicle `-32022`.

Ownership uses the live session repository's normalized email in the legacy
`Principal.phone` field, then an authoritative enabled `users._id` ObjectId lookup.
The client never chooses ownership (`userId` is rejected). Revoked/expired/disabled
sessions fail with established `-32001`. Strict existing `users`/`profile` schemas
are untouched. Each owner has an initially empty `user_garages` document with a
maximum of 100 stored vehicle snapshots, required source revision, request hash
and per-owner UUID. Insert + selection occur in a **single atomic Mongo update**,
with predicates preventing concurrent duplicates or exceeding the cap, plus a
unique `(owner,vehicles.clientRequestId)` index. Selection is owner-bound and
persisted in the same document; write failures never return false success.
Public catalog exports never query this private collection.

Targeted isolated integration validation (creates/drops only a random test DB):

```sh
cargo test -p quic-jsonrpc-server --lib mongo_catalog_garage_dispatcher_ownership_and_persistence -- --ignored
```
