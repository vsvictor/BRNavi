#!/bin/sh
set -eu

runtime=/run/brnavi-gateway
if [ -s "$runtime/fullchain.pem" ] && [ -s "$runtime/key.pem" ]; then
    export TLS_CERT_PATH="$runtime/fullchain.pem"
    export TLS_KEY_PATH="$runtime/key.pem"
fi

exec /usr/local/bin/json-rpc-gateway
