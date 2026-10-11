# BRNavi

Бекенд для мобільного клієнта BRNavi: сервіс `quic-jsonrpc-server` у Cargo-воркспейсі з бібліотечним крейтом
`map-tiles` (проксі тайлів) та **оркестратором** `brnavi`.

| Компонент | Роль | Порти (за замовч.) |
|---|---|---|
| `quic-jsonrpc-server` | JSON-RPC 2.0 over QUIC: логін водія, сесії, зміни (проксі до 1C driver-api), `map.getTile` | `8443/udp` |
| `map-tiles` | Бібліотека: проксі тайлів OSM з byte-bounded LRU-кешем і single-flight; транспорт-агностична | — |
| `quic-jsonrpc-client` | Rust-клієнт + C FFI (`.so` для Android/Flutter) | — |
| `brnavi` (корінь) | `brnavi` — оркестратор (`cargo run --bin brnavi`) | — |
| `offline-region-server/` | Окремий Python HTTP-сервіс пакетів офлайн-карт | `127.0.0.1:8400/tcp` |

## Запуск

```bash
make run          # quic-jsonrpc-server (звільняє порт, читає quic-jsonrpc-server/.env)
make stop         # зупинити сервер (за іменем бінарника та за портом)
make test         # cargo test --workspace
make lint         # fmt --check + clippy -D warnings (те саме, що CI)
```

Ті самі цілі є в `justfile`. Деплой (`deploy.sh`) збирає й запускає QUIC backend та окремий процес Python-сервера пакетів.

Або напряму: `cargo run --bin brnavi`. Вибір сервісів — `TMS_SERVICES=all|rpc`.

## Пакети офлайн-регіонів

Сервер `offline-region-server/server.py` — окремий Python 3.9+ процес; він працює поруч із QUIC backend і не потребує сторонніх Python-пакетів. Щоб розгорнути чи оновити лише його без перезапуску QUIC backend, використовуйте `DEPLOY_HOST=192.168.0.160 DEPLOY_USER=victor DEPLOY_DIR=/Users/victor/brnavi-runtime PACKAGE_SERVER_BIND=192.168.0.160 PACKAGE_SERVER_PORT=8400 ./deploy.sh --package-server-only`. Стан повного стека перевіряється через `./deploy.sh --status-only`. Каталог даних за замовчуванням — `/Users/victor/brnavi-runtime/offline-region-data` у цьому середовищі; він не перезаписується під час деплою.

За замовчуванням HTTP-сервер слухає лише loopback. У цьому середовищі його можна явно прив'язати до `192.168.0.160:8400` для доступу з локальної мережі. З'єднання тоді не шифрується і сервер не має автентифікації: використовуйте його лише в довіреній мережі. Для публічного або недовіреного доступу залиште loopback та налаштуйте HTTPS reverse proxy.

Пакети готуються окремо з OSM PBF та інструментами Valhalla. Скрипт підготовки й докладні вимоги до форматів наведено в `tools/offline-region-server/` репозиторію XRNavi. Після підготовки копіюйте пакет спочатку до віддаленого тимчасового каталогу, а потім атомарно переміщуйте в `<PACKAGE_DATA_DIR>/regions/<region-id>/<version>/`, щоб каталог не показав неповні файли. Див. [`offline-region-server/README.md`](offline-region-server/README.md).

## Конфігурація

Сервіс має **власний** `.env` поруч зі своїм `Cargo.toml`:

| Файл | Змінні | Приклад |
|---|---|---|
| `quic-jsonrpc-server/.env` | `QUIC_RPC_*` (QUIC bind, сесії, Mongo, TLS, rate-limit, `QUIC_RPC_RUST_LOG`), `HTTP_RPC_*` (upstream 1C driver-api: base URL, таймаут, retries), `MAP_TILE_*` (читає крейт `map-tiles`: upstream OSM тайл-сервер, User-Agent, ліміти, кеш) | `quic-jsonrpc-server/.env.example` |
| `.env` (корінь, необов'язково) | `TMS_*` оркестратора (`TMS_SERVICES`, `TMS_RUST_LOG`) + **fallback** для будь-якої змінної сервісу | `.env.example` |

**Пріоритет для кожної змінної:** змінна процесу → `<crate>/.env` → кореневий `.env`.
Якщо параметра немає у файлі крейту або він порожній (`KEY=`), значення береться з кореневого файлу.

Файли підхоплюються незалежно від cwd (`<crate>/.env`; для розгорнутих бінарників — `<crate-name>.env`
і `.env` поруч із виконуваним файлом). Відносні шляхи (`../certs/…`) рахуються від
директорії **того файлу, де вони задані**.

Оркестратор делегує завантаження крейту (`quic_jsonrpc_server::env`), потім читає
кореневий `.env` для `TMS_*`; фільтр логів — `TMS_RUST_LOG`, інакше збираються per-target директиви з
`QUIC_RPC_RUST_LOG`.

Креденшели Mongo (`QUIC_RPC_MONGO_USER`/`_PASSWORD` або повний `QUIC_RPC_MONGO_URI`) **обов'язкові** —
вбудованих значень немає.

## Протокол quic-jsonrpc-server

Newline-delimited JSON-RPC 2.0, один запит на bidi-стрім (рядок ≤ 64 KiB).

| Метод | Параметри | Примітка |
|---|---|---|
| `login` / `rpc.login` | `{phone, password, device{deviceId,…}}` | проксі до 1C; відповідь доповнюється `session.sessionId` |
| `rpc.resume` | `{sessionId}` | повертає `accessToken` для збереженої сесії |
| `rpc.session.info` | `{sessionId}` | без токена |
| `rpc.session.debug` | `{sessionId}` | лише при `QUIC_RPC_ENABLE_SESSION_DEBUG=true` |
| `logout` / `rpc.logout` | `{sessionId?}` | видаляє сесію; сервер доставляє відповідь і **закриває з'єднання** |
| `getShift` / `startShift` / `finishShift` | `{accessToken, …}` | проксі до 1C |
| `map.getTile` | `{z, x, y}` | тайл карти: `{z, x, y, contentType, etag, size, fromCache, data(base64)}` — див. нижче |
| `rpc.ping` / `rpc.health` / `echo` / `math.add` | — | службові |

**Життя з'єднання.** Сервер закриває QUIC-з'єднання лише на `rpc.logout` або коли клієнт став
**недосяжним**: від нього не прийшло жодного пакета (навіть ACK на keep-alive PING) протягом
`QUIC_RPC_IDLE_TIMEOUT_SECS` (60 с). Обидві сторони шлють QUIC keep-alive кожні `QUIC_RPC_KEEP_ALIVE_SECS`
(10 с), тому живий клієнт без прикладного трафіку цей таймаут ніколи не зачіпає; натомість телефон, що
зник без CONNECTION_CLOSE (NAT drop, Doze, зміна мережі), більше не висить «напіввідкритим» добу —
з'єднання прибирається за хвилину, а сесія лишається resumable на `QUIC_RPC_SESSION_TTL_SECS` (24 год).
Клієнт закриває сам (`quic_rpc_client_logout` — із видаленням сесії, `quic_rpc_client_close` — сесія
лишається resumable). Мертвий мережевий шлях клієнт виявляє heartbeat-ом (або QUIC idle timeout, 60 с)
і перепідключається з `rpc.resume` (0‑RTT). `QUIC_RPC_IDLE_TIMEOUT_SECS` має бути ≥ 3 × `QUIC_RPC_KEEP_ALIVE_SECS`.

Починаючи з `quic-jsonrpc-client` коміту `8be5bca`, heartbeat працює у strict-режимі ping-pong:
успіхом вважається лише валідна JSON-RPC-відповідь з `result == "pong"`; будь-який інший `Ok(...)`
трактується як heartbeat-failure і враховується в поріг reconnect.

Коди помилок: `-32700/-32600/-32601/-32602` (JSON-RPC), `-32000` storage, `-32001` session not found,
`-32002/-32003` upstream transport, `-32004` rate limited, `-3201x` upstream HTTP-статуси.

### Тайли карти (`map.getTile`)

Схема: **Flutter → JSON-RPC over QUIC → сервер → HTTP GET до OSM → сервер → base64 у JSON-RPC → Flutter**.

Upstream — локальний OSM тайл-сервер `http://192.168.1.242:8000/tiles/{z}/{x}/{y}.png` (plain HTTP;
значення перенесені з колишнього `h3-map-server/.env`, вони ж є дефолтами в коді).

* Сервер валідує `z ≤ MAP_TILE_MAX_ZOOM`, `x, y < 2^z` (`-32602`), ходить на `MAP_TILE_URL_TEMPLATE`
  з `MAP_TILE_USER_AGENT` (обов'язковий, якщо колись перемкнетесь на публічний
  [tile.openstreetmap.org](https://operations.osmfoundation.org/policies/tiles/)),
  обмежує розмір відповіді (`MAP_TILE_MAX_BYTES`), ретраїть 5xx/транспортні збої (`MAP_TILE_RETRIES`).
* Кеш у пам'яті: LRU за **байтами** (`MAP_TILE_CACHE_MAX_BYTES`, 0 — вимкнено), TTL
  (`MAP_TILE_CACHE_TTL_SECS`), single-flight — паралельні miss на один тайл роблять один upstream-запит.
  Помилки не кешуються. Лічильники — у `rpc.health` → `tiles.{cache_hits,cache_misses,upstream_errors}`.
* QUIC-спостережуваність у `rpc.health`: `quic.handshake.{success,failed,latency_avg_ms,latency_max_ms}`
  і `quic.disconnects.{peer_or_application_closed,locally_closed,timed_out,transport_error}`.
* `404` від OSM → `-32010 Tile not found`; завеликий тайл → `-32003`.
* Клієнт (`quic-jsonrpc-client`): `QuicJsonRpcClient::get_tile(z, x, y) -> TileImage` (байти вже
  декодовані) та FFI `quic_rpc_client_get_tile(client, z, x, y, &out_len, &out_content_type)`, що
  повертає **сирий буфер** (звільняти `quic_rpc_bytes_free(ptr, len)`; `out_content_type` —
  `quic_rpc_string_free`). Для Flutter це дає `Uint8List` без base64 на Dart-стороні.
* Уся тайлова логіка живе в крейті **`map-tiles`**; сервер лише валідує params, викликає
  `TileService` і мапить `TileError` → JSON-RPC-коди (`presentation/controller/map.rs`).

Мінімальний `TileProvider` для `flutter_map`:

```dart
class QuicTileProvider extends TileProvider {
  QuicTileProvider(this.rpc); // обгортка над quic_rpc_client_get_tile
  final QuicRpcClient rpc;

  @override
  ImageProvider getImage(TileCoordinates c, TileLayer options) =>
      _QuicTileImage(rpc, c.z, c.x, c.y);
}

class _QuicTileImage extends ImageProvider<_QuicTileImage> {
  _QuicTileImage(this.rpc, this.z, this.x, this.y);
  final QuicRpcClient rpc; final int z, x, y;

  @override
  Future<_QuicTileImage> obtainKey(ImageConfiguration _) => SynchronousFuture(this);

  @override
  ImageStreamCompleter loadImage(_QuicTileImage key, ImageDecoderCallback decode) =>
      MultiFrameImageStreamCompleter(
        codec: rpc.getTile(z, x, y)                // Future<Uint8List> через FFI
            .then((bytes) => ImmutableBuffer.fromUint8List(bytes))
            .then(decode),
        scale: 1,
      );

  @override
  bool operator ==(Object o) => o is _QuicTileImage && o.z == z && o.x == x && o.y == y;
  @override
  int get hashCode => Object.hash(z, x, y);
}
```

Виклик FFI робіть в ізоляті/`compute`, бо `quic_rpc_client_get_tile` блокує потік до відповіді.

## Безпека та ліміти

* `sessionId` — 192-бітний CSPRNG (base64url); це bearer-credential для `rpc.resume`.
* `login`/`rpc.resume` обмежені двоярусним token-bucket: м'яко per-IP (`QUIC_RPC_AUTH_RATE_PER_MINUTE`/`_BURST`, дефолт 120/60 — мобільні клієнти за carrier-NAT ділять IP) і жорстко per-identity — телефон для `login`, sessionId для `rpc.resume` (`QUIC_RPC_AUTH_IDENTITY_RATE_PER_MINUTE`/`_BURST`, дефолт 10/5).
* Ліміти розмірів: рядок запиту 64 KiB; QUIC `max_incoming`; тайл — `MAP_TILE_MAX_BYTES`, кеш тайлів — `MAP_TILE_CACHE_MAX_BYTES`.
* Логи: `password`, `*token*`, `sessionId`, `authorization` редагуються; телефон маскується.
  У release-збірці клієнт не отримує внутрішні URL/тіла помилок upstream.
* Сертифікат має містити SAN, з яким підключається клієнт (`make gen-certs CERT_CN=… CERT_SANS=…`;
  CLI: `--server-name` / `QUIC_RPC_SERVER_NAME`).

## Якість

```bash
just lint          # cargo fmt --check + clippy -D warnings
cargo test --workspace
cargo deny check   # advisories / licenses / bans / sources (deny.toml)
```

CI (`.github/workflows/ci.yml`) виконує те саме на кожному push/PR. Toolchain — `rust-toolchain.toml`
(stable, MSRV 1.88 через `rust-version`).

## Архітектура

Усі крейти побудовані за **Clean Architecture**; серверна презентація — за **MVC**. Залежності
спрямовані лише всередину: `presentation → application → domain ← infrastructure`.

```
<crate>/src/
├── domain/          сутності, value objects, помилки, порти (traits) — без I/O
├── application/     use cases (сервіси), метрики — оперують лише портами
├── infrastructure/  адаптери: конфіг з env (`config`), пошук власного .env (`env`), TLS, HTTP до upstream, БД, транспорт (QUIC)
├── presentation/    MVC
│   ├── model.rs     DTO/валідація вхідних параметрів
│   ├── view.rs      рендер відповіді (JSON-RPC envelope)
│   └── controller/  контролери за ресурсами + front controller (RpcDispatcher)
└── lib.rs           composition root: збирає адаптери → сервіси → контролери
```

### quic-jsonrpc-server

* `domain`: `Session`, `Principal`, `RpcError` (коди), порти `SessionRepository`, `DriverApiGateway`.
* `application`: `AuthService` (login/resume/info/debug/logout), `ShiftService`, `RateLimiter`, `Metrics`.
* `infrastructure`: `config` (`ServerOptions`, `UpstreamOptions`, `MongoOptions`), `persistence`
  (`InMemorySessionRepository`, `MongoSessionRepository`), `http::HttpDriverApiGateway`
  (retry/backoff для всіх upstream-викликів), `quic` (транспорт), `tls`, `logging`.
* `presentation`: `model` (`LoginParams`, `StartShiftParams`, `TileParams`, …), `view::JsonRpcView`,
  `controller` (`AuthController`, `ShiftController`, `MapController` → `map-tiles`, `SystemController`, `RpcDispatcher`).
* Unit-тести використовують `FakeDriverApi` / `FakeTileSource` — у продакшн-коді немає `cfg(test)`-гілок.

### map-tiles

* `domain`: `TileCoord` (валідація `z`/`x`/`y`), `Tile`, `TileError`, порт `TileSource`.
* `application::TileService`: LRU-кеш за байтами + TTL, single-flight, `evict_expired`, `cache_usage`.
* `infrastructure`: `TileOptions::from_env()` (`MAP_TILE_*`, дефолти = локальний OSM `192.168.1.242:8000`),
  `OsmTileSource` (reqwest, User-Agent, ліміт розміру, ретраї transient-помилок).
* `map_tiles::from_env()` — зручний конструктор для хоста. Жодних залежностей від JSON-RPC/QUIC.
