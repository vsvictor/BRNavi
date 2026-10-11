#!/usr/bin/env bash

###############################################################################
#                    BRNavi Deployment Script                                 #
#                                                                             #
# Usage:                                                                      #
#   ./deploy.sh [--no-build] [--status-only] [--package-server-only]        #
#                                                                             #
# Options:                                                                    #
#   --no-build      Skip build phase, use existing binary                   #
#   --status-only   Only check server status on remote host                 #
#   --package-server-only Deploy/restart only the offline HTTP sidecar       #
#                                                                             #
# Environment:                                                                #
#   DEPLOY_HOST     Target host (default: 192.168.1.132)                    #
#   DEPLOY_USER     SSH user (default: dev)                                 #
#   DEPLOY_PASS     SSH password (will prompt if not set)                   #
#   SSH_PORT        SSH port (default: 22)
#   DEPLOY_PORT     QUIC server port (default: 8443)                        #
#   DEPLOY_DIR      Remote deploy directory (default: /home/dev/brnavi)  #
#   PACKAGE_SERVER_PORT  Offline package HTTP port (default: 8400)
#   PACKAGE_SERVER_BIND  Offline package bind address (default: 192.168.0.160)
#   PACKAGE_DATA_DIR      Persistent offline package directory
#   QUIC_CHECK_TIMEOUT      Timeout for health-check command (default: 12s)  #
#   QUIC_CHECK_SERVERNAME   SNI for health-check (default: tms.express-t.ua) #
#   QUIC_HEALTH_CA_CERT     Local CA PEM path for quic-jsonrpc-client        #
#   QUIC_HEALTH_REQUEST     JSON-RPC request used for health-check           #
#   QUIC_HEALTH_RETRIES     Number of retries for QUIC health-check          #
#   QUIC_HEALTH_RETRY_DELAY Delay between retries in seconds                 #
#                                                                             #
###############################################################################

set -euo pipefail

# Color codes for output
readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly YELLOW='\033[1;33m'
readonly BLUE='\033[0;34m'
readonly CYAN='\033[0;36m'
readonly NC='\033[0m' # No Color

# Configuration
readonly DEPLOY_HOST="${DEPLOY_HOST:-192.168.1.132}"
readonly DEPLOY_USER="${DEPLOY_USER:-dev}"
readonly SSH_PORT="${SSH_PORT:-22}"
readonly DEPLOY_PORT="${DEPLOY_PORT:-8443}"
readonly DEPLOY_DIR="${DEPLOY_DIR:-/home/dev/brnavi}"
readonly QUIC_CHECK_TIMEOUT="${QUIC_CHECK_TIMEOUT:-12}"
# The production certificate is issued for *.express-t.ua, so the health-check must present a
# matching SNI even though it connects by IP.
readonly QUIC_CHECK_SERVERNAME="${QUIC_CHECK_SERVERNAME:-tms.express-t.ua}"
readonly QUIC_CLIENT_TIMEOUT_MS="${QUIC_CLIENT_TIMEOUT_MS:-5000}"
readonly QUIC_HEALTH_CA_CERT="${QUIC_HEALTH_CA_CERT:-quic-jsonrpc-client/certs/ca-root.pem}"
readonly QUIC_HEALTH_REQUEST_DEFAULT='{"jsonrpc":"2.0","method":"rpc.ping","id":"deploy-health","params":{}}'
readonly QUIC_HEALTH_REQUEST="${QUIC_HEALTH_REQUEST:-$QUIC_HEALTH_REQUEST_DEFAULT}"
readonly QUIC_HEALTH_RETRIES="${QUIC_HEALTH_RETRIES:-2}"
readonly QUIC_HEALTH_RETRY_DELAY="${QUIC_HEALTH_RETRY_DELAY:-2}"
readonly BINARY_NAME="quic-jsonrpc-server"
readonly LOCAL_BINARY="target/release/$BINARY_NAME"
readonly REMOTE_BINARY="$DEPLOY_DIR/$BINARY_NAME"
# The server loads `quic-jsonrpc-server.env` next to the executable with top priority (it
# replaces the compile-time crate path); a plain `.env` would only be a low-priority fallback.
readonly REMOTE_ENV="$DEPLOY_DIR/$BINARY_NAME.env"
readonly REMOTE_LEGACY_ENV="$DEPLOY_DIR/.env"
readonly LOCAL_ENV="quic-jsonrpc-server/.env"
readonly LOCAL_CERT_DIR="certs"
readonly REMOTE_CERT_DIR="$DEPLOY_DIR/certs"
readonly CERT_FILES=("server-fullchain.pem" "server-key.pem")
readonly PACKAGE_SERVER_DIR="$DEPLOY_DIR/offline-region-server"
readonly PACKAGE_SERVER_SCRIPT="$PACKAGE_SERVER_DIR/server.py"
readonly PACKAGE_SERVER_PIDFILE="$PACKAGE_SERVER_DIR/server.pid"
readonly PACKAGE_SERVER_LOG="$PACKAGE_SERVER_DIR/server.log"
readonly PACKAGE_DATA_DIR="${PACKAGE_DATA_DIR:-$DEPLOY_DIR/offline-region-data}"
readonly PACKAGE_SERVER_BIND="${PACKAGE_SERVER_BIND:-192.168.0.160}"
readonly PACKAGE_SERVER_PORT="${PACKAGE_SERVER_PORT:-8400}"

# Parse arguments
NO_BUILD=false
STATUS_ONLY=false
PACKAGE_SERVER_ONLY=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build)
      NO_BUILD=true
      shift
      ;;
    --status-only)
      STATUS_ONLY=true
      shift
      ;;
    --package-server-only)
      PACKAGE_SERVER_ONLY=true
      shift
      ;;
    *)
      echo "Unknown option: $1"
      exit 1
      ;;
  esac
done

# Print colored output
log_info() {
  echo -e "${CYAN}[INFO]${NC} $*"
}

log_success() {
  echo -e "${GREEN}[✓]${NC} $*"
}

log_warning() {
  echo -e "${YELLOW}[⚠]${NC} $*"
}

log_error() {
  echo -e "${RED}[✗]${NC} $*" >&2
}

print_stage() {
  echo ""
  echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
  echo -e "${BLUE}$1${NC}"
  echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

# ...existing code...
ssh_exec() {
  local cmd="$1"
  local -a ssh_args=(-n -p "$SSH_PORT" -o ConnectTimeout=10 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)
  if [ -n "${DEPLOY_PASS:-}" ]; then
    command -v sshpass >/dev/null 2>&1 || { log_error "sshpass is required when DEPLOY_PASS is set"; return 1; }
    sshpass -p "$DEPLOY_PASS" ssh "${ssh_args[@]}" "$DEPLOY_USER@$DEPLOY_HOST" "$cmd"
  else
    ssh "${ssh_args[@]}" "$DEPLOY_USER@$DEPLOY_HOST" "$cmd"
  fi
}

ssh_copy() {
  local src="$1"
  local dst="$2"
  local -a scp_args=(-P "$SSH_PORT" -o ConnectTimeout=10 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)
  if [ -n "${DEPLOY_PASS:-}" ]; then
    command -v sshpass >/dev/null 2>&1 || { log_error "sshpass is required when DEPLOY_PASS is set"; return 1; }
    sshpass -p "$DEPLOY_PASS" scp "${scp_args[@]}" "$src" "$DEPLOY_USER@$DEPLOY_HOST:$dst"
  else
    scp "${scp_args[@]}" "$src" "$DEPLOY_USER@$DEPLOY_HOST:$dst"
  fi
}

# Check SSH connectivity
check_ssh_connection() {
  print_stage "Checking SSH Connectivity"

  log_info "Host: $DEPLOY_HOST"
  log_info "User: $DEPLOY_USER"

  if ssh_exec "echo 'SSH connection successful'" > /dev/null 2>&1; then
    log_success "SSH connection successful"
  else
    log_error "Failed to connect via SSH"
    exit 1
  fi
}

# Get SSH password if not set
get_ssh_password() {
  if [ -z "${DEPLOY_PASS:-}" ]; then
    if ssh -n -p "$SSH_PORT" -o BatchMode=yes -o ConnectTimeout=5 \
      -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      "$DEPLOY_USER@$DEPLOY_HOST" true >/dev/null 2>&1; then
      log_info "Using SSH key authentication"
      return
    fi
    if ! command -v sshpass >/dev/null 2>&1; then
      log_error "SSH key authentication failed and sshpass is unavailable; configure an SSH key or install sshpass for password authentication"
      exit 1
    fi
    read -sp "Enter SSH password for $DEPLOY_USER: " DEPLOY_PASS
    echo ""
  fi
  export DEPLOY_PASS
}

run_with_timeout() {
  local duration="$1"
  shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "${duration}s" "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "${duration}s" "$@"
  else
    python3 -c '
import os
import signal
import subprocess
import sys

process = subprocess.Popen(sys.argv[2:], start_new_session=True)
try:
    result = process.wait(timeout=float(sys.argv[1]))
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    sys.exit(124)
sys.exit(result)
' "$duration" "$@"
  fi
}

# Active QUIC health-check from local machine (deploy host) to remote endpoint.
check_quic_handshake() {
  log_info "Running QUIC client health-check from local machine to ${DEPLOY_HOST}:${DEPLOY_PORT} (SNI: ${QUIC_CHECK_SERVERNAME})"

  if ! command -v cargo >/dev/null 2>&1; then
    log_warning "'cargo' command is not available, QUIC health-check failed"
    return 1
  fi

  if [ ! -f "$QUIC_HEALTH_CA_CERT" ]; then
    log_warning "CA certificate not found for QUIC health-check: $QUIC_HEALTH_CA_CERT"
    return 1
  fi

  local quic_output=""
  local quic_rc=0
  local total_attempts=$((QUIC_HEALTH_RETRIES + 1))
  local attempt=1

  while [ "$attempt" -le "$total_attempts" ]; do
    quic_output=$(run_with_timeout "$QUIC_CHECK_TIMEOUT" \
      cargo run --quiet -p quic-jsonrpc-client -- \
      "${DEPLOY_HOST}:${DEPLOY_PORT}" \
      "$QUIC_HEALTH_CA_CERT" \
      "$QUIC_HEALTH_REQUEST" \
      --server-name "${QUIC_CHECK_SERVERNAME}" \
      --timeout-ms "${QUIC_CLIENT_TIMEOUT_MS}" 2>&1) || quic_rc=$?

    if [ "$quic_rc" -eq 0 ]; then
      if [ "$attempt" -gt 1 ]; then
        log_success "QUIC client handshake/request succeeded on attempt ${attempt}/${total_attempts}"
      else
        log_success "QUIC client handshake/request succeeded"
      fi
      return 0
    fi

    if [ "$quic_rc" -eq 124 ]; then
      log_warning "QUIC handshake timed out on attempt ${attempt}/${total_attempts} after ${QUIC_CHECK_TIMEOUT}s"
    else
      log_warning "QUIC client health-check failed on attempt ${attempt}/${total_attempts} (exit code: ${quic_rc})"
    fi

    echo "$quic_output" | tail -n 15 | sed 's/^/  /' || true

    if [ "$attempt" -lt "$total_attempts" ]; then
      log_info "Retrying QUIC health-check in ${QUIC_HEALTH_RETRY_DELAY}s..."
      sleep "$QUIC_HEALTH_RETRY_DELAY"
    fi

    quic_rc=0
    attempt=$((attempt + 1))
  done

  return 1
}

# Check server status
check_server_status() {
  print_stage "Checking Server Status on Remote Host"
  local status_failed=0

  log_info "Checking if $BINARY_NAME is running on $DEPLOY_HOST:$DEPLOY_PORT..."

  # Check if process exists
  local pids
  pids=$(ssh_exec "pidof '$BINARY_NAME' || true" 2>/dev/null)

  if [ -n "$pids" ]; then
    log_success "Server process found (PIDs: $pids)"

    # Check port
    local port_check
    port_check=$(ssh_exec "ss -lntup | grep -E ':$DEPLOY_PORT\\b' || echo 'not listening'" 2>/dev/null)

    if ! echo "$port_check" | grep -q "not listening"; then
      log_success "Server is bound on port $DEPLOY_PORT (TCP/UDP check)"
      log_info "Port binding info:"
      echo "$port_check" | sed 's/^/  /'
    else
      log_warning "Server is running but port $DEPLOY_PORT is not bound"
      status_failed=1
    fi

    # Show process info
    local proc_info
    proc_info=$(ssh_exec "ps aux | grep '$BINARY_NAME' | grep -v grep || true" 2>/dev/null)
    if [ -n "$proc_info" ]; then
      log_info "Process information:"
      echo "$proc_info" | sed 's/^/  /'
    fi
  else
    log_warning "Server process not found running on remote host"
    status_failed=1
  fi

  # Check binary file
  if ssh_exec "[ -f '$REMOTE_BINARY' ]" 2>/dev/null; then
    local binary_info
    binary_info=$(ssh_exec "ls -lh '$REMOTE_BINARY' && file '$REMOTE_BINARY'" 2>/dev/null)
    log_info "Binary file info:"
    echo "$binary_info" | sed 's/^/  /'
  else
    log_warning "Binary file not found at $REMOTE_BINARY"
  fi

  # Check config
  if ssh_exec "[ -f '$REMOTE_ENV' ]" 2>/dev/null; then
    log_success "Configuration found on remote host ($REMOTE_ENV)"
  else
    log_warning "Configuration NOT found on remote host ($REMOTE_ENV)"
  fi

  # Active end-to-end check from deploy host to QUIC listener.
  if ! check_quic_handshake; then
    status_failed=1
  fi

  print_stage "Checking Offline Package Server"
  if ssh_exec "pidfile='$PACKAGE_SERVER_PIDFILE'; script='$PACKAGE_SERVER_SCRIPT'; [ -r \"\$pidfile\" ] || exit 1; pid=\$(cat \"\$pidfile\"); case \"\$pid\" in ''|*[!0-9]*) exit 1;; esac; kill -0 \"\$pid\" 2>/dev/null && ps -p \"\$pid\" -o args= | grep -F -- \"\$script\" >/dev/null" 2>/dev/null; then
    log_success "Offline package server process found"
    local health_host="$PACKAGE_SERVER_BIND"
    if [ "$health_host" = "0.0.0.0" ]; then health_host="127.0.0.1"; fi
    if ssh_exec "python3 -c 'import urllib.request; assert urllib.request.urlopen(\"http://$health_host:$PACKAGE_SERVER_PORT/healthz\", timeout=3).status == 200'" 2>/dev/null; then
      log_success "Offline package server health check succeeded on port $PACKAGE_SERVER_PORT"
    else
      log_warning "Offline package server is running but health check failed"
      status_failed=1
    fi
  else
    log_warning "Offline package server is not running"
    status_failed=1
  fi

  if [ "$status_failed" -ne 0 ]; then
    return 1
  fi
}

# Build the project
build_project() {
  print_stage "Building BRNavi Project"

  log_info "Current directory: $(pwd)"
  log_info "Build command: make run"

  if [ ! -f "Makefile" ]; then
    log_error "Makefile not found in current directory"
    exit 1
  fi

  log_info "This will compile the Rust project. This may take several minutes..."

  # The 'make run' command both builds AND runs the server
  # For deployment, we just need to build it
  # We'll use cargo build instead

  log_info "Building with cargo..."
  if cargo build --release -p quic-jsonrpc-server 2>&1 | tee /tmp/build.log; then
    log_success "Build completed successfully"
  else
    log_error "Build failed. See log above for details."
    exit 1
  fi

  if [ ! -f "$LOCAL_BINARY" ]; then
    log_error "Binary not found at $LOCAL_BINARY after build"
    exit 1
  fi

  local binary_size
  binary_size=$(du -h "$LOCAL_BINARY" | cut -f1)
  log_success "Binary ready: $LOCAL_BINARY ($binary_size)"
}

# Prepare remote directory
prepare_remote() {
  print_stage "Preparing Remote Host"

  log_info "Creating deployment directory: $DEPLOY_DIR"
  ssh_exec "mkdir -p '$DEPLOY_DIR' '$PACKAGE_SERVER_DIR' '$PACKAGE_DATA_DIR'"
  if ! ssh_exec "python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)'"; then
    log_error "Python 3.9 or newer is required on the remote host"
    exit 1
  fi

  log_success "Remote directory ready"
}

# Stop existing server
stop_remote_server() {
  print_stage "Stopping Existing Server"

  log_info "Stopping any existing $BINARY_NAME process..."

  local stop_result
  # Avoid matching the remote shell command itself; stop only exact process names.
  stop_result=$(ssh_exec "pidof '$BINARY_NAME' >/dev/null && pkill -f '/$BINARY_NAME( |$)' || echo 'no process found'" 2>&1)

  if echo "$stop_result" | grep -q "no process found"; then
    log_info "No running server found"
  else
    log_success "Server process stopped"
    log_info "Waiting for port to be released..."
    sleep 2
  fi

  stop_package_server
}

stop_package_server() {
  log_info "Stopping the offline package server if it is already running..."
  ssh_exec "pidfile='$PACKAGE_SERVER_PIDFILE'; script='$PACKAGE_SERVER_SCRIPT'; if [ -r \"\$pidfile\" ]; then pid=\$(cat \"\$pidfile\"); case \"\$pid\" in ''|*[!0-9]*) rm -f \"\$pidfile\";; *) if kill -0 \"\$pid\" 2>/dev/null && ps -p \"\$pid\" -o args= | grep -F -- \"\$script\" >/dev/null; then kill -TERM \"\$pid\"; sleep 1; fi; rm -f \"\$pidfile\";; esac; fi"
}

# Deploy binary
deploy_binary() {
  print_stage "Deploying Binary"

  if [ ! -f "$LOCAL_BINARY" ]; then
    log_error "Local binary not found: $LOCAL_BINARY"
    exit 1
  fi

  local binary_size
  binary_size=$(du -h "$LOCAL_BINARY" | cut -f1)

  log_info "Source: $LOCAL_BINARY ($binary_size)"
  log_info "Target: $REMOTE_BINARY"

  log_info "Copying binary to remote host..."
  ssh_copy "$LOCAL_BINARY" "$DEPLOY_DIR/"

  log_info "Making binary executable..."
  ssh_exec "chmod +x '$REMOTE_BINARY'"

  log_success "Binary deployed successfully"
}

# Deploy the independent Python package-server process.
deploy_package_server() {
  print_stage "Deploying Offline Package Server"

  if [ ! -f "offline-region-server/server.py" ]; then
    log_error "Offline package server source not found: offline-region-server/server.py"
    exit 1
  fi

  ssh_copy "offline-region-server/server.py" "$PACKAGE_SERVER_SCRIPT"
  ssh_exec "chmod 644 '$PACKAGE_SERVER_SCRIPT'"
  log_success "Offline package server deployed to $PACKAGE_SERVER_SCRIPT"
  log_info "Package data remains in $PACKAGE_DATA_DIR and is not overwritten by deployments"
}

# Deploy configuration
deploy_config() {
  print_stage "Deploying Configuration"

  if [ ! -f "$LOCAL_ENV" ]; then
    log_warning "Local .env not found: $LOCAL_ENV"
    log_info "Using remote config if it exists, or server will use defaults"
    return 0
  fi

  # Certificates: shipped next to the binary so the deployment is self-contained and does not
  # depend on a source checkout existing on the target host.
  log_info "Copying TLS certificates to $REMOTE_CERT_DIR..."
  ssh_exec "mkdir -p '$REMOTE_CERT_DIR' && chmod 700 '$REMOTE_CERT_DIR'"
  for cert in "${CERT_FILES[@]}"; do
    if [ ! -f "$LOCAL_CERT_DIR/$cert" ]; then
      log_error "Certificate not found: $LOCAL_CERT_DIR/$cert"
      exit 1
    fi
    ssh_copy "$LOCAL_CERT_DIR/$cert" "$REMOTE_CERT_DIR/$cert"
  done
  ssh_exec "chmod 600 '$REMOTE_CERT_DIR'/*.pem"

  # Relative cert paths are resolved against the directory of the env file that set them, so
  # point them at the certs we just shipped (the dev file uses ../certs relative to the crate).
  local rendered_env
  rendered_env=$(mktemp)
  sed -E \
    -e 's#^(QUIC_RPC_CERT_PATH=).*#\1certs/server-fullchain.pem#' \
    -e 's#^(QUIC_RPC_KEY_PATH=).*#\1certs/server-key.pem#' \
    "$LOCAL_ENV" > "$rendered_env"

  log_info "Source: $LOCAL_ENV"
  log_info "Target: $REMOTE_ENV"

  log_info "Copying config to remote host..."
  ssh_copy "$rendered_env" "$REMOTE_ENV"
  rm -f "$rendered_env"
  # Older deployments shipped the config as `.env`, which the server treats as a low-priority
  # fallback — remove it so there is exactly one source of truth.
  ssh_exec "rm -f '$REMOTE_LEGACY_ENV'" || true

  log_info "Environment variables in deployed config:"
  ssh_exec "grep -v '^#' '$REMOTE_ENV' | grep -v '^$' | sed -E 's/^(QUIC_RPC_MONGO_PASSWORD=).*/\\1***/'" | sed 's/^/  /' || true

  log_success "Configuration deployed successfully"
}

# Start remote server
start_remote_server() {
  print_stage "Starting Server on Remote Host"

  log_info "Starting $BINARY_NAME on $DEPLOY_HOST..."
  log_info "Server will listen on port $DEPLOY_PORT"

  # Start server in background, fully detached from the ssh session. Even so, some sshd/pty
  # combinations keep the session open until the child exits — bound the call with `timeout`
  # (the server itself is unaffected; we verify it below via pidof / port / QUIC check).
  ssh_exec "cd '$DEPLOY_DIR' && (setsid nohup ./$BINARY_NAME > server.log 2>&1 < /dev/null &) ; exit 0" \
    >/dev/null 2>&1 || true

  log_info "Waiting for server to start..."
  sleep 3

  # Check if server started successfully
  if ssh_exec "pidof '$BINARY_NAME' > /dev/null" 2>/dev/null; then
    log_success "Server process started successfully"
  else
    log_error "Server process did not start"
    log_info "Checking remote server log..."
    ssh_exec "tail -20 '$DEPLOY_DIR/server.log'" || true
    exit 1
  fi

  # Check if port is listening
  sleep 2
  if ssh_exec "ss -lntup | grep -Eq ':$DEPLOY_PORT\\b'" 2>/dev/null; then
    log_success "Server is bound on port $DEPLOY_PORT"
  else
    log_warning "Server process found but port $DEPLOY_PORT is not bound yet"
    log_info "This may be normal if the server is still initializing"
  fi

  if ! check_quic_handshake; then
    log_error "Server started, but QUIC health-check failed"
    log_info "Checking remote server log..."
    ssh_exec "tail -40 '$DEPLOY_DIR/server.log'" || true
    exit 1
  fi
}

# Start and health-check the HTTP sidecar. It is bound to loopback by default so only an HTTPS
# reverse proxy should expose it to mobile clients.
start_package_server() {
  print_stage "Starting Offline Package Server"
  log_info "Starting HTTP package server on $PACKAGE_SERVER_BIND:$PACKAGE_SERVER_PORT"
  local health_host="$PACKAGE_SERVER_BIND"

  if [[ ! "$PACKAGE_SERVER_PORT" =~ ^[0-9]+$ ]] || (( PACKAGE_SERVER_PORT < 1 || PACKAGE_SERVER_PORT > 65535 )); then
    log_error "PACKAGE_SERVER_PORT must be between 1 and 65535"
    exit 1
  fi
  if [[ ! "$PACKAGE_SERVER_BIND" =~ ^[A-Za-z0-9.:_-]+$ ]]; then
    log_error "PACKAGE_SERVER_BIND must be a host name or IP address"
    exit 1
  fi
  if [[ ! "$PACKAGE_DATA_DIR" =~ ^/[A-Za-z0-9._/-]+$ ]]; then
    log_error "PACKAGE_DATA_DIR must be a simple absolute path"
    exit 1
  fi
  if [ "$health_host" = "0.0.0.0" ]; then health_host="127.0.0.1"; fi

  ssh_exec "mkdir -p '$PACKAGE_DATA_DIR' '$PACKAGE_SERVER_DIR' && (nohup python3 -u '$PACKAGE_SERVER_SCRIPT' --data-dir '$PACKAGE_DATA_DIR' --host '$PACKAGE_SERVER_BIND' --port '$PACKAGE_SERVER_PORT' > '$PACKAGE_SERVER_LOG' 2>&1 < /dev/null & echo \$! > '$PACKAGE_SERVER_PIDFILE')"

  for attempt in 1 2 3 4 5 6 7 8 9 10; do
    if ssh_exec "python3 -c 'import urllib.request; assert urllib.request.urlopen(\"http://$health_host:$PACKAGE_SERVER_PORT/healthz\", timeout=2).status == 200'" 2>/dev/null; then
      log_success "Offline package server is healthy"
      return 0
    fi
    sleep 1
  done

  log_error "Offline package server failed to start; recent log output:"
  ssh_exec "tail -40 '$PACKAGE_SERVER_LOG'" || true
  exit 1
}

# Show server logs
show_server_logs() {
  print_stage "Server Startup Logs"

  log_info "Last 30 lines of server log:"
  echo ""
  ssh_exec "tail -30 '$DEPLOY_DIR/server.log'" || true
  log_info "Last 30 lines of offline package server log:"
  ssh_exec "tail -30 '$PACKAGE_SERVER_LOG'" || true
}

# Show final status
show_final_status() {
  print_stage "Deployment Summary"

  log_success "Deployment completed successfully!"

  echo ""
  echo "Server Details:"
  echo "  Host: $DEPLOY_HOST"
  echo "  User: $DEPLOY_USER"
  echo "  Port: $DEPLOY_PORT"
  echo "  Binary: $REMOTE_BINARY"
  echo "  Config: $REMOTE_ENV"
  echo "  Logs: $DEPLOY_DIR/server.log"
  echo "  Offline package server: http://$PACKAGE_SERVER_BIND:$PACKAGE_SERVER_PORT"
  echo "  Package data: $PACKAGE_DATA_DIR"
  echo "  Package server logs: $PACKAGE_SERVER_LOG"
  echo ""

  echo "To check server status:"
  echo "  ssh $DEPLOY_USER@$DEPLOY_HOST \"ps aux | grep $BINARY_NAME\""
  echo "  ssh $DEPLOY_USER@$DEPLOY_HOST \"tail -f $DEPLOY_DIR/server.log\""
  echo ""

  echo "To manually stop the server:"
  echo "  ssh $DEPLOY_USER@$DEPLOY_HOST \"if [ -r '$PACKAGE_SERVER_PIDFILE' ]; then kill \\\$(cat '$PACKAGE_SERVER_PIDFILE'); fi\" # offline package server"
  echo "  ssh $DEPLOY_USER@$DEPLOY_HOST \"pkill -f $BINARY_NAME\" # QUIC backend"
  echo ""
}

# Main execution
main() {
  print_stage "BRNavi Deployment Script"
  echo "Target: $DEPLOY_HOST:$DEPLOY_PORT"
  echo "Date: $(date)"
  echo ""

  if [[ "$STATUS_ONLY" != true ]]; then
    if [[ ! "$PACKAGE_SERVER_PORT" =~ ^[0-9]+$ ]] || (( PACKAGE_SERVER_PORT < 1 || PACKAGE_SERVER_PORT > 65535 )); then
      log_error "PACKAGE_SERVER_PORT must be between 1 and 65535"
      exit 1
    fi
    if [[ ! "$PACKAGE_SERVER_BIND" =~ ^[A-Za-z0-9.:_-]+$ ]] || [[ ! "$PACKAGE_DATA_DIR" =~ ^/[A-Za-z0-9._/-]+$ ]]; then
      log_error "Invalid package server bind address or data directory"
      exit 1
    fi
  fi

  # Get password first
  get_ssh_password

  # Status only mode
  if [ "$STATUS_ONLY" = true ]; then
    check_ssh_connection
    check_server_status
    exit 0
  fi

  if [ "$PACKAGE_SERVER_ONLY" = true ]; then
    check_ssh_connection
    prepare_remote
    deploy_package_server
    stop_package_server
    start_package_server
    log_success "Offline package server is available on $PACKAGE_SERVER_BIND:$PACKAGE_SERVER_PORT (HTTPS reverse proxy not configured)"
    exit 0
  fi

  # Full deployment flow
  check_ssh_connection

  if [ "$NO_BUILD" = false ]; then
    build_project
  else
    log_info "Skipping build phase (--no-build flag)"
    if [ ! -f "$LOCAL_BINARY" ]; then
      log_error "Binary not found and --no-build flag was used"
      exit 1
    fi
  fi

  prepare_remote
  stop_remote_server
  deploy_binary
  deploy_package_server
  deploy_config
  start_remote_server
  start_package_server
  show_server_logs

  # Give server time to fully initialize
  sleep 3

  # Final status check
  check_server_status
  show_final_status
}

# Handle errors
trap 'log_error "Deployment failed at line $LINENO"' ERR

# Execute main function
main "$@"
