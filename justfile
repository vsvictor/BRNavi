set shell := ["bash", "-cu"]

default:
    @just --list

# The service reads its own env file automatically: quic-jsonrpc-server/.env
# (QUIC_RPC_*, HTTP_RPC_*, MAP_TILE_*).
run:
    cargo run -p quic-jsonrpc-server

stop:
    make stop

test:
    cargo test --workspace

lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Delegates to the Makefile so both entry points produce the same certificate (CN/SANs).
# Override: just gen-certs CERT_CN=my.host CERT_SANS=DNS:my.host,IP:10.0.0.5
gen-certs *ARGS:
    make gen-certs {{ARGS}}

# Verifies key/leaf match, chain order and validation against quic-jsonrpc-client/certs/ca-root.pem.
check-certs:
    make check-certs

clean-certs:
    make clean-certs
