#!/bin/sh
set -eu

instance=${1:?backend instance number is required}
case "$instance" in
    *[!0-9]*|'') echo "invalid backend instance number" >&2; exit 2 ;;
esac
port=$((18080 + instance))
tls_port=$((18442 + instance))
runtime="/run/brnavi-backend-$instance"

umask 077
openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
    -keyout "$runtime/backend-key.pem" \
    -out "$runtime/backend-cert.pem" \
    -subj "/CN=brnavi-backend-$instance" \
    -addext "subjectAltName=DNS:brnavi-backend-$instance"

export BACKEND_ID="brnavi-backend-$instance"
export BACKEND_BIND=127.0.0.1
export BACKEND_PORT="$port"
export BACKEND_TCP_PORT="$port"
export BACKEND_UDP_PORT="$port"
export BACKEND_TLS_PORT="$tls_port"
export BACKEND_QUIC_PORT="$tls_port"
export GATEWAY_CONTROL_ADDR=127.0.0.1:8500
export BACKEND_TLS_CERT_FILE="$runtime/backend-cert.pem"
export BACKEND_TLS_KEY_FILE="$runtime/backend-key.pem"
exec /usr/local/bin/brnavi
