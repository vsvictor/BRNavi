#!/bin/sh
set -eu
umask 077

command -v openssl >/dev/null 2>&1 || {
    echo "OpenSSL is required to generate authentication secrets." >&2
    exit 1
}
if [ ! -e .env ]; then
    cp .env.example .env
fi
chmod 0600 .env

if ! grep -q '^AUTH_DB_PASSWORD=.' .env; then
    printf '\nAUTH_DB_PASSWORD=%s\n' "$(openssl rand -hex 32)" >> .env
fi
if ! grep -q '^AUTH_DB_ROOT_PASSWORD=.' .env; then
    printf '\nAUTH_DB_ROOT_PASSWORD=%s\n' "$(openssl rand -hex 32)" >> .env
fi
if ! grep -q '^PASETO_LOCAL_KEY=.' .env; then
    key=$(openssl rand -base64 32)
    key=$(printf '%s' "$key" | tr '+/' '-_' | tr -d '=\n')
    printf '\nPASETO_LOCAL_KEY=k4.local.%s\n' "$key" >> .env
fi
