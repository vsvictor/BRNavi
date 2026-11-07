#!/bin/sh
set -eu

tls_dir=${BRNAVI_TLS_DIR:-/home/victor/.ssh/brnavi-tls}

command -v docker >/dev/null 2>&1 || {
    echo "Docker Engine is required; install Docker and the Compose plugin first." >&2
    exit 1
}
command -v openssl >/dev/null 2>&1 || {
    echo "OpenSSL is required to create the SRT passphrase." >&2
    exit 1
}
docker compose version >/dev/null
sudo docker info >/dev/null

sh docker/init-auth-env.sh

for file in mobilespace_app.pem mobilespace_app_bundle.pem mobilespace_app_key.pem; do
    if [ ! -s "$tls_dir/$file" ]; then
        echo "Missing TLS deployment file: $tls_dir/$file" >&2
        exit 1
    fi
done

chmod 0700 "$tls_dir"
chmod 0600 "$tls_dir/mobilespace_app_key.pem"

if ! grep -q '^GATEWAY_SRT_PASSPHRASE=.' .env; then
    printf 'GATEWAY_SRT_PASSPHRASE=%s\n' "$(openssl rand -hex 20)" >> .env
fi
if ! grep -q '^BRNAVI_BROKER_PASSWORD=.' .env; then
    printf 'BRNAVI_BROKER_PASSWORD=%s\n' "$(openssl rand -hex 24)" >> .env
fi
chmod 0600 .env

if grep -q '^GATEWAY_PUBLISH_ADDRESS=' .env; then
    sed -i 's/^GATEWAY_PUBLISH_ADDRESS=.*/GATEWAY_PUBLISH_ADDRESS=0.0.0.0/' .env
else
    printf '\nGATEWAY_PUBLISH_ADDRESS=0.0.0.0\n' >> .env
fi

sudo docker compose \
    -f docker-compose.yml \
    -f docker-compose.tls.yml \
    build

sudo install -m 0644 docker/brnavi.service /etc/systemd/system/brnavi.service
sudo systemctl daemon-reload
sudo systemctl enable brnavi.service
sudo systemctl restart brnavi.service
