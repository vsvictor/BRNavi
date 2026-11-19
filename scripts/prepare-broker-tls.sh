#!/bin/sh
set -eu

service=${1:?broker service name is required}
case "$service" in
    mosquitto)
        runtime=/run/brnavi-mosquitto
        account=mosquitto
        ;;
    rabbitmq)
        runtime=/run/brnavi-rabbitmq
        account=rabbitmq
        ;;
    *)
        echo "unsupported broker service: $service" >&2
        exit 2
        ;;
esac

source_dir=${BRNAVI_TLS_DIR:-/home/victor/.ssh/brnavi-tls}
for file in mobilespace_app.pem mobilespace_app_bundle.pem mobilespace_app_key.pem; do
    test -s "$source_dir/$file" || {
        echo "Missing TLS deployment file: $source_dir/$file" >&2
        exit 1
    }
done

install -d -o "$account" -g "$account" -m 0750 "$runtime"
cat "$source_dir/mobilespace_app.pem" > "$runtime/fullchain.pem"
awk '
    /-----BEGIN CERTIFICATE-----/ { certificate++ }
    certificate <= 2 { print }
    certificate == 3 { exit }
' "$source_dir/mobilespace_app_bundle.pem" >> "$runtime/fullchain.pem"
cp "$source_dir/mobilespace_app_key.pem" "$runtime/key.pem"
chown "$account:$account" "$runtime/fullchain.pem" "$runtime/key.pem"
chmod 0444 "$runtime/fullchain.pem"
chmod 0400 "$runtime/key.pem"
