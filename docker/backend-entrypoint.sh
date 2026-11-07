#!/bin/sh
set -eu

umask 077
openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
    -keyout /tmp/backend-key.pem \
    -out /tmp/backend-cert.pem \
    -subj "/CN=${BACKEND_ID:-brnavi-backend}" \
    -addext "subjectAltName=DNS:${BACKEND_ID:-brnavi-backend}"

export BACKEND_TLS_CERT_FILE=/tmp/backend-cert.pem
export BACKEND_TLS_KEY_FILE=/tmp/backend-key.pem
exec /usr/local/bin/brnavi
