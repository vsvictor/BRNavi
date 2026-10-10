SHELL := /bin/bash

.PHONY: run stop test lint gen-certs check-certs clean-certs android-client

ANDROID_CLIENT_PROFILE ?= release
ANDROID_CLIENT_ABIS ?= arm64-v8a

# The service reads its own env file automatically: quic-jsonrpc-server/.env
# (QUIC_RPC_* listener/sessions/TLS, HTTP_RPC_* 1C upstream, MAP_TILE_* tile proxy).
RPC_ENV := quic-jsonrpc-server/.env

# Port used by quic-jsonrpc-server (QUIC_RPC_BIND in $(RPC_ENV))
RPC_PORT ?= $(shell sed -n 's/^QUIC_RPC_BIND=.*:\([0-9]\+\)[[:space:]]*$$/\1/p' $(RPC_ENV) 2>/dev/null | head -n1)
RPC_PORT := $(if $(RPC_PORT),$(RPC_PORT),8443)

# TLS identity for gen-certs. The client must connect with a server name matching one of the SANs
# (quic-jsonrpc-client --server-name / QUIC_RPC_SERVER_NAME; Android passes it to quic_rpc_client_connect).
CERT_CN ?= mobilespace.app
CERT_SANS ?= DNS:$(CERT_CN),DNS:localhost,IP:127.0.0.1,IP:192.168.0.160

android-client:
	bash ./tool/build_rust_android.sh --profile "$(ANDROID_CLIENT_PROFILE)" --abis "$(ANDROID_CLIENT_ABIS)"

# JSON-RPC over QUIC server. Reads its own $(RPC_ENV) automatically; frees the port first.
run:
	@port="$(RPC_PORT)"; \
	pids="$$(ss -lunpH "sport = :$$port" 2>/dev/null | sed -n 's/.*pid=\([0-9]\+\).*/\1/p' | sort -u | tr '\n' ' ')"; \
	if [ -n "$$pids" ]; then \
		echo "Port $$port is busy (PID(s): $$pids). Stopping old instance(s)..."; \
		kill $$pids 2>/dev/null || true; \
		sleep 1; \
		leftover="$$(ss -lunpH "sport = :$$port" 2>/dev/null | sed -n 's/.*pid=\([0-9]\+\).*/\1/p' | sort -u | tr '\n' ' ')"; \
		if [ -n "$$leftover" ]; then \
			echo "Force stopping PID(s): $$leftover"; \
			kill -9 $$leftover 2>/dev/null || true; \
		fi; \
	fi; \
	cargo run -p quic-jsonrpc-server

# Stops a running server: by binary name (cargo run / deployed) and by whatever holds its UDP port.
stop:
	@stopped=0; \
	pids="$$(pgrep -f "target/[a-z]*/quic-jsonrpc-server( |$$)" | tr '\n' ' ')"; \
	if [ -n "$$pids" ]; then \
		echo "Stopping quic-jsonrpc-server (PID(s): $$pids)..."; \
		kill $$pids 2>/dev/null || true; \
		stopped=1; \
	fi; \
	pids="$$(ss -lunpH "sport = :$(RPC_PORT)" 2>/dev/null | sed -n 's/.*pid=\([0-9]\+\).*/\1/p' | sort -u | tr '\n' ' ')"; \
	if [ -n "$$pids" ]; then \
		echo "Port $(RPC_PORT) still held by PID(s): $$pids. Stopping..."; \
		kill $$pids 2>/dev/null || true; \
		stopped=1; \
	fi; \
	if [ "$$stopped" = 1 ]; then \
		sleep 1; \
		leftover="$$(ss -lunpH "sport = :$(RPC_PORT)" 2>/dev/null | sed -n 's/.*pid=\([0-9]\+\).*/\1/p' | sort -u | tr '\n' ' ')"; \
		if [ -n "$$leftover" ]; then \
			echo "Force stopping PID(s) on port $(RPC_PORT): $$leftover"; \
			kill -9 $$leftover 2>/dev/null || true; \
		fi; \
		echo "Server stopped."; \
	else \
		echo "Nothing to stop: quic-jsonrpc-server is not running."; \
	fi

test:
	cargo test --workspace

# Same checks as CI (.github/workflows/ci.yml).
lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

# Self-signed *development* identity. Refuses to overwrite a CA-issued (non self-signed) certificate
# unless FORCE=1: certs/server-fullchain.pem + server-key.pem hold the production Sectigo chain.
# (`openssl x509 -in <pem>` reads the first certificate of a bundle, i.e. the leaf.)
CERT_FULLCHAIN := certs/server-fullchain.pem
CERT_KEY := certs/server-key.pem
define REFUSE_IF_CA_ISSUED
	@if [ -z "$(FORCE)" ] && [ -f $(CERT_FULLCHAIN) ] && \
	   [ "$$(openssl x509 -in $(CERT_FULLCHAIN) -noout -subject)" != "$$(openssl x509 -in $(CERT_FULLCHAIN) -noout -issuer)" ]; then \
	  echo "$(CERT_FULLCHAIN) is CA-issued (not self-signed); refusing to $(1). Use FORCE=1 to override."; exit 1; fi
endef

gen-certs:
	@command -v openssl >/dev/null 2>&1 || { echo "openssl is required"; exit 1; }
	$(call REFUSE_IF_CA_ISSUED,overwrite)
	mkdir -p certs
	openssl req -x509 -newkey rsa:2048 -sha256 -nodes -days 365 \
		-subj "/CN=$(CERT_CN)" \
		-addext "basicConstraints=critical,CA:FALSE" \
		-addext "keyUsage=critical,digitalSignature,keyEncipherment" \
		-addext "extendedKeyUsage=serverAuth" \
		-addext "subjectAltName=$(CERT_SANS)" \
		-keyout $(CERT_KEY) \
		-text \
		-out $(CERT_FULLCHAIN)
	chmod 600 $(CERT_KEY)
	@echo "Generated $(CERT_FULLCHAIN) and $(CERT_KEY) (self-signed, CN=$(CERT_CN), SANs=$(CERT_SANS))"
	@echo "Set QUIC_RPC_CERT_PATH/QUIC_RPC_KEY_PATH=../certs/... in $(RPC_ENV) to use them"
	@echo "Self-signed: clients must trust $(CERT_FULLCHAIN) itself (not quic-jsonrpc-client/certs/ca-root.pem)"

# Verifies the on-disk TLS layout: key matches the leaf (first cert of the fullchain) and the chain
# validates against the client trust anchor (or against itself when self-signed).
check-certs:
	@set -e; \
	  a="$$(openssl x509 -in $(CERT_FULLCHAIN) -noout -pubkey | openssl pkey -pubin -outform DER | sha256sum)"; \
	  b="$$(openssl pkey -in $(CERT_KEY) -pubout -outform DER | sha256sum)"; \
	  [ "$$a" = "$$b" ] && echo "key matches leaf: OK" || { echo "key does NOT match leaf"; exit 1; }; \
	  echo "fullchain: $$(grep -c 'BEGIN CERT' $(CERT_FULLCHAIN)) cert(s)"; \
	  openssl x509 -in $(CERT_FULLCHAIN) -noout -subject -issuer -ext subjectAltName -enddate; \
	  leaf="$$(openssl x509 -in $(CERT_FULLCHAIN))"; \
	  if [ "$$(openssl x509 -in $(CERT_FULLCHAIN) -noout -subject)" = "$$(openssl x509 -in $(CERT_FULLCHAIN) -noout -issuer)" ]; then \
	    echo "$$leaf" | openssl verify -CAfile $(CERT_FULLCHAIN); \
	  else \
	    echo "$$leaf" | openssl verify -CAfile quic-jsonrpc-client/certs/ca-root.pem -untrusted $(CERT_FULLCHAIN); fi

clean-certs:
	$(call REFUSE_IF_CA_ISSUED,delete)
	rm -f $(CERT_FULLCHAIN) $(CERT_KEY)
