# Сервер пакетів офлайн-регіонів

Цей окремий Python HTTP-сервіс працює поруч із QUIC-сервером BRNavi й обслуговує готові пакети офлайн-карт. Для роботи потрібен Python 3.9+ та стандартна бібліотека Python. Сервіс не створює і не завантажує картографічні дані.

## Розгортання разом із BRNavi

Скрипт `../deploy.sh --package-server-only` копіює `server.py` на сервер і запускає його окремим процесом від QUIC backend, не перезапускаючи backend. Потрібен Python 3.9+ на цільовому хості.

Типові налаштування:

- адреса прослуховування: `192.168.0.160` (доступ з довіреної локальної мережі; за потреби перевизначте `PACKAGE_SERVER_BIND`, зокрема на `127.0.0.1` для доступу лише з localhost);
- порт: `8400`;
- каталог даних: `$DEPLOY_DIR/offline-region-data`;
- журнал і PID: `$DEPLOY_DIR/offline-region-server/`.

Для ізоляції артефактів розгортання від Git-копії задайте `DEPLOY_DIR` як окремий каталог поза checkout. Змінні `PACKAGE_SERVER_BIND`, `PACKAGE_SERVER_PORT` та `PACKAGE_DATA_DIR` у середовищі локального `deploy.sh` дають змогу перевизначити відповідні значення. Каталог даних зберігається між розгортаннями й не перезаписується скриптом.

Сервер надає звичайний HTTP і за замовчуванням прив'язаний до приватної IP-адреси локальної мережі (`192.168.0.160`), щоб бути доступним довіреним клієнтам у LAN. Щоб обмежити доступ лише localhost-ом, перевизначте `PACKAGE_SERVER_BIND=127.0.0.1`. **Не виставляйте його напряму в недовірену мережу:** трафік не шифрується, автентифікації немає. Для публічного доступу використовуйте loopback та HTTPS reverse proxy на хості.

## Підготовка й публікація регіону

Підготовку виконують окремо від сервера. Скрипт `tools/offline-region-server/prepare_region.py` у репозиторії XRNavi бере OSM PBF та конфігурацію Valhalla й створює версійований пакет. Потрібні інструменти збирання Valhalla; для витягування адрес із OSM також установіть PyOsmium (`python3 -m pip install osmium`). Детальні вимоги до вхідних даних наведено в інструкції XRNavi.

Після підготовки пакета скопіюйте його до тимчасового каталогу на цільовому хості, а потім атомарно перемістіть у `regions/`. Не копіюйте напряму в активний каталог `regions/`: сервер може інакше побачити неповний пакет.

```sh
# Заповніть значення для вашого регіону й хоста.
REGION_ID=us-wa-seattle
VERSION=2026.10.09
OUTPUT_ROOT=/srv/offline-regions
REMOTE=dev@your-server.example
REMOTE_DIR=/home/dev/brnavi/offline-region-data

ssh "$REMOTE" "mkdir -p '$REMOTE_DIR/staging/$REGION_ID/$VERSION'"
rsync -a "$OUTPUT_ROOT/regions/$REGION_ID/$VERSION/" \
  "$REMOTE:$REMOTE_DIR/staging/$REGION_ID/$VERSION/"
ssh "$REMOTE" "mkdir -p '$REMOTE_DIR/regions/$REGION_ID' && test ! -e '$REMOTE_DIR/regions/$REGION_ID/$VERSION' && mv '$REMOTE_DIR/staging/$REGION_ID/$VERSION' '$REMOTE_DIR/regions/$REGION_ID/$VERSION'"
```

Версії незмінні: для оновлення створюйте нову версію, не перезаписуйте вже опубліковану. Під час публікації застосовуйте ту саму OpenStreetMap attribution та дотримуйтеся вимог ODbL для похідних даних.

Після публікації перевірте локально на сервері:

```sh
curl --fail http://127.0.0.1:8400/healthz
curl --fail http://127.0.0.1:8400/v1/catalog
```

Каталог даних містить `regions/<region-id>/<version>/manifest.json`, `tiles.tar.gz` та `address-index.sqlite`. Клієнт перевіряє цілісність SHA-256 і обмежує розмір кожного артефакту 512 MiB.
