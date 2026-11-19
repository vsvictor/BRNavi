#!/bin/sh
set -eu

source_dir=${BRNAVI_TLS_DIR:-/home/victor/.ssh/brnavi-tls}
runtime=/run/brnavi-gateway

for file in mobilespace_app.pem mobilespace_app_bundle.pem mobilespace_app_key.pem; do
    test -s "$source_dir/$file" || {
        echo "Missing TLS deployment file: $source_dir/$file" >&2
        exit 1
    }
done

cat "$source_dir/mobilespace_app.pem" > "$runtime/fullchain.pem"
awk '
    /-----BEGIN CERTIFICATE-----/ { certificate++ }
    certificate <= 2 { print }
    certificate == 3 { exit }
' "$source_dir/mobilespace_app_bundle.pem" >> "$runtime/fullchain.pem"
cp "$source_dir/mobilespace_app_key.pem" "$runtime/key.pem"
chown -R brnavi-gateway:brnavi-gateway "$runtime"
chmod 0750 "$runtime"
chmod 0444 "$runtime/fullchain.pem"
chmod 0400 "$runtime/key.pem"
