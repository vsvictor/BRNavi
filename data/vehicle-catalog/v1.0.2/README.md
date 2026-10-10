# BRNavi vehicle catalog — v1.0.2

**Database license: Open Database License (ODbL) 1.0.** Data from
[vehicle-makes-models](https://github.com/gor3a/vehicle-makes-models), maintained by
Mina Sameh / gor3a, originally sourced from [autoevolution.com](https://www.autoevolution.com).
BRNavi's changes are described below. This database and BRNavi's adapted database
are offered under ODbL 1.0, not under BRNavi's proprietary application-code license.

## Preserved source and legal notices

- `vehicle-data-json.zip`: original release archive, unmodified, from
  <https://github.com/gor3a/vehicle-makes-models/releases/download/v1.0.2/vehicle-data-json.zip>.
- SHA-256: `76e524ef5b26c733d2ec911df706f0dec0dcc28f4032f261f0ecf139ba10db8e`.
- `LICENSE-DATA`: complete upstream attribution/data-license notice from tag `v1.0.2`.
- `ODbL-1.0.html`: complete legally binding ODbL 1.0 text downloaded from
  <https://opendatacommons.org/licenses/odbl/1-0/>. Retain this file when redistributing.
- `LICENSE-CODE-MIT`: upstream code license, separate from the database license.
  No upstream scraper code is included, executed, or needed by BRNavi.

## Adaptation and schema (schemaVersion 1)

The CLI emits UTF-8 newline-delimited JSON, one object per line:

| File | Records |
|---|---:|
| `adapted-makes.jsonl` | 164 |
| `adapted-models.jsonl` | 2,649 |
| `adapted-generations.jsonl` | 7,217 |
| `adapted-variants.jsonl` | 30,535 |

`adapted-metadata.json` describes provenance, license, units and public retrieval.
Each entity has `id`, `name`, nullable `parentId`, nullable `yearStart` / `yearEnd`,
`sourcePath` and `version`. Make parents are null; models reference makes;
generations reference models. Make/model years are null because BRNavi does not
expose or invent generation bounds at those hierarchy levels.

Variants have `id`, `makeId`, `modelId`, `generationId`, `make`, `model`,
`generation`, `engine`, nullable `yearStart`, `yearEnd`, `lengthMm`, `widthMm`,
`heightMm`, `curbWeightKg`, plus `sourcePath` and `version`. Length/width/height
are millimetres, curb mass is kilograms. A null is unknown, never a default.

IDs are full SHA-256 hashes of `v1.0.2:<archive-file>#/<JSON-array-path>`, prefixed
`make-`, `model-`, `generation-` or `variant-`. Source array indexes preserve
duplicate display names (424 repeated generation names and 570 repeated
make/model/generation/engine labels), without merging distinct source records.
IDs are collision-checked and deterministic for this pinned release; no stability
across future releases is promised. The `all.json` aggregate is excluded.

The source has 65 implausible measurements. BRNavi converts measurements outside
these ranges to null: length 100–50,000 mm, width/height 100–10,000 mm, curb mass
1–200,000 kg. Missing values also remain null. The original source values remain
available in the original archive. No scraping, estimation, inferred dimensions,
or conversion from free-text specifications is performed.

## Import and public availability

From the workspace root:

```sh
cargo run -p quic-jsonrpc-server --bin import-catalog -- --validate-only
cargo run -p quic-jsonrpc-server --bin import-catalog
```

The first command validates the pinned digest, parses the whole archive, checks
record counts/collisions and writes exports, without accessing Mongo. The second
does the same and explicitly imports only public catalog collections into the
configured Mongo database. Startup never downloads/imports the catalog. Existing
private `users`, `profile`, `sessions` and garage contents are never imported,
exported, mixed with catalog data or cleared. Re-import of the verified, complete
same release is idempotent. Incomplete imports have no ready marker and public
reads fail with `-32020` rather than returning fake or partial catalogs. A crashed
CLI's `catalog_vehicle_metadata` document `_id: "import-lock"` must be inspected
and explicitly removed by an operator before retrying.

For deployed binaries, copy this directory (including legal notices) to a
persistent directory and run `import-catalog --data-dir /path/to/v1.0.2`.
The directory is for **public catalog artifacts only**, not secrets/private DB dumps.

**Internal files alone do not fulfill a public share-alike offer.** The server
implements unauthenticated `catalog.license` and `catalog.export` on its existing
public QUIC JSON-RPC interface. No additional HTTP listener is introduced.
`catalog.license` returns this attribution, the canonical license URL, version,
archive digest, units, and a complete public-export retrieval mechanism.

To retrieve the **entire** adapted database, for each `kind` (`makes`, `models`,
`generations`, `variants`) call:

```json
{"jsonrpc":"2.0","id":1,"method":"catalog.export","params":{"kind":"variants","limit":100}}
```

Then repeat with the returned `nextCursor` as `cursor` until it is null. Every
page contains `{schemaVersion,version,kind,license,items,nextCursor}`. `items`
use exactly the JSONL schemas above, contain no Mongo internal `_id` or owner/
session/garage fields, and the union of all four kinds is the complete adapted
database. Default `kind` is `variants`; default limit is 50, maximum 100.
Public reads are IP-rate-limited (300/minute sustained, burst 100); follow normal
retry guidance on `-32004`. No authentication, payment, account registration, or
garage ownership is needed to obtain the licensed public catalog.

**Deployment obligation:** expose these public RPC methods to recipients of the
catalog/produced work and publish the actual QUIC endpoint and retrieval
instructions with the attribution notice (for example in the client About
screen). Verify retrieval from outside the deployment. Alternatively also serve
the unmodified archive, exports and notices as static public downloads. This
checkout does not claim that an unreachable/internally deployed endpoint or
internal artifact directory alone fulfills the public availability obligation.
