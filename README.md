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

### Authentication plugin

Both instances install the pinned [JSON-RPC-2-OAuth2](https://github.com/vsvictor/JSON-RPC-2-OAuth2)
plugin. MongoDB 8.0 stores users, profiles and RBAC in the persistent
`auth-mongo-data` volume; its port is not published. Both instances use the same
`PASETO_LOCAL_KEY` and `AUTH_ISSUER`. The application authenticates as `brnavi`
with `readWrite` access only to `brnavi_auth`, not as the MongoDB administrator.
To open an authenticated interactive shell on the BRNavi host, run
`./docker/mongo-shell.sh` from any directory inside the project; the script
uses the root password already available inside the MongoDB container without
printing or passing it as a command-line argument.

Run `sh docker/init-auth-env.sh` from the project root before starting Compose.
It creates `.env` if absent, restricts it to mode `0600`, and generates the
application and administrator database passwords and PASETO v4.local key only when missing. TLS deployment
runs this automatically. Keep `.env` and database backups safe; do not delete
or regenerate passwords while reusing the MongoDB volume. Rotating the
PASETO key invalidates existing tokens. Secrets are never committed or baked
into images.

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

Run this in `./mongo-shell.sh`, then click **Refresh list** in XRCenter. The
role appears in the edit dropdown for administrators with `rbac.roles.write`.

Example requests, sent through an encrypted transport:

```json
{"jsonrpc":"2.0","id":1,"method":"auth.register","params":{"email":"user@example.test","password":"<strong-password>","profile":{}}}
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
passwords, access/refresh tokens, and profile fields. Docker retains up to five
10 MB log files per backend. Restrict access to `docker logs` and the Docker
daemon; do not attach raw logs to public tickets.

### Start locally

```sh
sh docker/init-auth-env.sh
docker compose up --build
```

The gateway accepts TCP and UDP on port `8443` by default, published on
`127.0.0.1` only. To connect a physical phone on a trusted LAN, set
`GATEWAY_PUBLISH_ADDRESS` in `.env` to the host's LAN IP and restart Compose.
Do not expose this starter directly to the public internet: this initial setup
does not enforce authentication for all methods or provide production TLS certificates.

Example JSON-RPC request (the response's `result` should alternate between the
two instance IDs as requests are balanced):

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"identify","params":{}}' | nc 127.0.0.1 8443
```

Useful commands:

```sh
docker compose ps
docker compose logs -f gateway backend-1 backend-2
docker compose down
```

The gateway's Control Plane and Prometheus metrics listener are container-only
and are not published to the host. Backend TLS/QUIC listeners are enabled by
the upstream backend builder but remain internal to the Compose network.

### TLS and QUIC deployment

The public domain certificate belongs on the gateway, which terminates client
TLS and QUIC; it is not copied into the two backend instances. The RustRover
profile maps the leaf certificate, GoDaddy CA bundle, and matching private key
from `$USER_HOME$/Certs/mobilespace.app` to
`/home/victor/.ssh/brnavi-tls` on the server. These files are outside the
repository and excluded from Git and Docker build contexts.

After uploading the project and certificate files, run on the server:

```sh
cd /home/victor/brnavi
sh docker/deploy-with-tls.sh
```

Run it from an interactive SSH session so `sudo` can prompt for the server
administrator password when installing the system service.

The deployment script creates local-only secrets in `.env`, installs and enables
the `brnavi.service` system service, builds the stack, then starts it. The
gateway uses host networking and listens on TCP and UDP `8443`, sharing the
port across all 30 upstream transports, including TLS, QUIC, HTTP,
datagram, and WebSocket variants.
It also starts MQTT over TLS on TCP `8883` and AMQP over TLS on TCP `5671`;
those brokers require the generated username/password in `.env`. Their
unencrypted gateway-to-broker connections are bound to loopback only. Plain
TCP and UDP transports are enabled on the shared `8443` port as requested, so
clients may use unencrypted protocols there as well as TLS/QUIC. The deployment
firewall rule blocks the gateway Control Plane on TCP `8500`.
Use `sudo systemctl status brnavi`, `sudo systemctl restart brnavi`, or
`sudo journalctl -u brnavi -f` to manage it. Docker's restart policies also
bring containers back after a Docker daemon or server restart. At
container startup, the gateway and broker containers copy the leaf certificate,
both intermediate certificates, and the private key into temporary
`/run/brnavi-tls` and `/run/brnavi-broker` filesystems. The gateway runs as an unprivileged user and
broker private-key files are restricted to their respective services. The
private key is not baked into an image or stored in the repository. Renewed
files must be uploaded and the deployment script run again.

DCCP, SCTP and UDP-Lite are enabled on the shared listener, but external
firewalls must allow IP protocols 33, 132 and 136 (not TCP/UDP ports).
These transports require kernel support on both the server and client. MQTT and
AMQP broker endpoints use TLS and require the generated credentials; their
plaintext local ports are loopback-only. SRT and WebSocket/SRT clients also
need the generated `GATEWAY_SRT_PASSPHRASE` from `.env`. The server or provider firewall must
allow TCP and UDP `8443`, TCP `8883` and `5671`, and IP protocols 33, 132 and 136.
The deployment can install a host nftables rule but cannot configure an
external provider firewall. TLS authenticates the server and broker endpoints,
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

### RustRover SFTP profile

The project includes a manual SFTP deployment profile under `.idea`. In
RustRover, open **Tools → Deployment → Browse Remote Host** to connect to
`95.216.88.141` as `victor`; use **Tools → Deployment → Upload to Mobilespace**
to upload the mapped project files to `/home/victor/brnavi`. The profile uses
`$USER_HOME$/.ssh/id_rsa` and does not store a password. RustRover may prompt
for the key passphrase. The profile intentionally does not auto-upload changes
or include `.env` secrets. After upload, start or update the Compose stack
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
MongoDB 8.0 зберігає користувачів, профілі й RBAC у постійному volume
`auth-mongo-data`; порт БД не публікується. Інстанси використовують спільні
`PASETO_LOCAL_KEY` та `AUTH_ISSUER`. Застосунок підключається як `brnavi`
із правами `readWrite` лише для `brnavi_auth`, не як адміністратор MongoDB.
Щоб відкрити інтерактивну автентифіковану консоль на сервері BRNavi,
виконайте `./docker/mongo-shell.sh` з будь-якого каталогу всередині проєкту.
Скрипт використовує root-пароль, доступний у контейнері MongoDB, не виводячи
його та не передаючи аргументом командного рядка.

Перед запуском Compose виконайте `sh docker/init-auth-env.sh` із кореня
проєкту. Скрипт створює `.env`, якщо його немає, встановлює права `0600` та
генерує окремі паролі застосунку й адміністратора БД та PASETO v4.local ключ
лише за їх відсутності. TLS-деплой
виконує це автоматично. Бережіть `.env` і резервні копії БД; не видаляйте та
не генеруйте повторно паролі для вже створеного MongoDB volume. Зміна
PASETO-ключа робить наявні токени невалідними. Секрети не потрапляють у Git
чи Docker-образи.

MongoDB використовує окремий volume від попередньої конфігурації PostgreSQL.
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

Виконайте команду у консолі `./mongo-shell.sh`, потім натисніть
**«Оновити список»** у XRCenter. Роль з’явиться у випадаючому списку
адміністраторів із дозволом `rbac.roles.write`.

Приклади запитів через зашифрований транспорт:

```json
{"jsonrpc":"2.0","id":1,"method":"auth.register","params":{"email":"user@example.test","password":"<strong-password>","profile":{}}}
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
паролі, access/refresh-токени й поля профілю. Docker зберігає до п'яти файлів
журналу по 10 МБ для кожного бекенду. Обмежте доступ до `docker logs` і Docker
daemon; не додавайте такі журнали до публічних звернень.

### Локальний запуск

```sh
sh docker/init-auth-env.sh
docker compose up --build
```

Шлюз приймає TCP та UDP на порту `8443` і за замовчуванням публікує його лише
на `127.0.0.1`. Щоб під'єднати фізичний телефон у довіреній локальній мережі,
вкажіть IP комп'ютера в цій мережі у `GATEWAY_PUBLISH_ADDRESS` файлу `.env` і
перезапустіть Compose. Не відкривайте цей початковий варіант безпосередньо в
публічний інтернет: не всі методи вимагають автентифікації, а локальний
профіль не має production TLS-сертифікатів.

Приклад JSON-RPC-запиту (значення `result` має змінюватися між ID двох
інстансів у міру балансування запитів):

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"identify","params":{}}' | nc 127.0.0.1 8443
```

Корисні команди:

```sh
docker compose ps
docker compose logs -f gateway backend-1 backend-2
docker compose down
```

Control Plane шлюзу та Prometheus metrics-listener доступні лише всередині
контейнерної мережі й не публікуються на хост. TLS/QUIC-listener-и бекенду
вмикаються upstream builder-ом, але залишаються внутрішніми для Compose.

### Деплой TLS та QUIC

Сертифікат публічного домену встановлюється на шлюзі, який завершує TLS- та
QUIC-з'єднання мобільних клієнтів; до двох інстансів бекенду він не
копіюється. Профіль RustRover зіставляє сертифікат домену, CA bundle GoDaddy
та відповідний приватний ключ із `$USER_HOME$/Certs/mobilespace.app` у
`/home/victor/.ssh/brnavi-tls` на сервері. Файли зберігаються поза
репозиторієм і виключені з Git та Docker build context.

Після завантаження файлів проєкту й сертифікатів запустіть на сервері:

```sh
cd /home/victor/brnavi
sh docker/deploy-with-tls.sh
```

Запускайте скрипт в інтерактивній SSH-сесії, щоб `sudo` могло запросити
пароль адміністратора сервера для встановлення системного сервісу.

Скрипт створить локальні секрети у `.env`, встановить і ввімкне сервіс
`brnavi.service`, збере стек і запустить його. Шлюз використовує host networking
та слухає TCP і UDP `8443`, розділяючи порт між доступними TLS, QUIC, HTTP,
датаграмними й WebSocket-варіантами — усіма 30 upstream-транспортами.
Додатково запускаються MQTT over TLS на TCP `8883` та AMQP over TLS на TCP
`5671`; для брокерів потрібні згенеровані логін і пароль із `.env`.
Незашифровані з'єднання від шлюзу до брокерів доступні лише через loopback.
Звичайні TCP і UDP також увімкнені на спільному порту `8443`, тому клієнти
можуть використовувати незашифровані протоколи поряд із TLS/QUIC. Firewall
блокує Control Plane шлюзу на TCP `8500`.
Керування:
`sudo systemctl status brnavi`, `sudo systemctl restart brnavi` або
`sudo journalctl -u brnavi -f`. Політики перезапуску Docker також піднімуть
контейнери після перезапуску Docker чи сервера. Під час старту
контейнер шлюзу та брокерів копіює сертифікат, обидва проміжні сертифікати й
приватний ключ у тимчасові файлові системи `/run/brnavi-tls` та
`/run/brnavi-broker`. Шлюз працює без привілеїв; ключі брокерів доступні лише
відповідним сервісам. Приватний ключ не вбудовується в образ і не зберігається
в репозиторії. Після поновлення сертифіката завантажте нові файли та повторно
запустіть скрипт деплою.

DCCP, SCTP і UDP-Lite увімкнені на спільному listener-і, але зовнішній firewall
має дозволяти IP-протоколи 33, 132 і 136 (це не TCP/UDP-порти). Ці транспорти
потребують підтримки ядром і сервера, і клієнта. Брокери MQTT та AMQP
працюють через TLS і потребують згенерованих облікових даних; локальні
незашифровані порти доступні лише через loopback. Клієнтам SRT і
WebSocket/SRT також потрібен згенерований `GATEWAY_SRT_PASSPHRASE` із `.env`.
Firewall сервера або
провайдера має дозволяти TCP і UDP `8443`, TCP `8883` і `5671`, а також
IP-протоколи 33, 132 і 136. Скрипт може додати правило nftables на сервері, але не
налаштовує зовнішній firewall провайдера. TLS автентифікує сервер і брокерні
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

### Профіль SFTP у RustRover

У `.idea` додано профіль ручного SFTP-деплою. У RustRover відкрийте
**Tools → Deployment → Browse Remote Host**, щоб під'єднатися до
`95.216.88.141` як `victor`; **Tools → Deployment → Upload to Mobilespace**
завантажує зіставлені файли проєкту до `/home/victor/brnavi`. Профіль
використовує `$USER_HOME$/.ssh/id_rsa` і не зберігає пароль. RustRover може
запросити парольну фразу ключа. Автоматичне завантаження вимкнене, `.env`
із секретами не включено. Після передачі файлів Compose-стек потрібно
окремо запустити або оновити на сервері. Значення `https://mobilespace.app`
використане лише як веб-URL профілю: воно не налаштовує DNS, TLS-проксі чи
HTTPS для шлюзу.
