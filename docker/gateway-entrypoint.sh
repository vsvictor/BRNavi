#!/bin/sh
set -eu

source_dir=/run/certificate-source
runtime_dir=/run/brnavi-tls

if [ -d "$source_dir" ]; then
    test -s "$source_dir/mobilespace_app.pem"
    test -s "$source_dir/mobilespace_app_bundle.pem"
    test -s "$source_dir/mobilespace_app_key.pem"

    mkdir -p "$runtime_dir"
    cat "$source_dir/mobilespace_app.pem" > "$runtime_dir/fullchain.pem"
    awk '
        /-----BEGIN CERTIFICATE-----/ { certificate++ }
        certificate <= 2 { print }
        certificate == 3 { exit }
    ' "$source_dir/mobilespace_app_bundle.pem" >> "$runtime_dir/fullchain.pem"
    cp "$source_dir/mobilespace_app_key.pem" "$runtime_dir/key.pem"

    chown -R 10001:10001 "$runtime_dir"
    chmod 0500 "$runtime_dir"
    chmod 0444 "$runtime_dir/fullchain.pem"
    chmod 0400 "$runtime_dir/key.pem"

    export TLS_CERT_PATH="$runtime_dir/fullchain.pem"
    export TLS_KEY_PATH="$runtime_dir/key.pem"
fi

exec setpriv --reuid=10001 --regid=10001 --clear-groups \
    /usr/local/bin/json-rpc-gateway
