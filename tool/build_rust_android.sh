#!/usr/bin/env bash
set -euo pipefail

# Build Rust QUIC JSON-RPC client for Android and copy .so files to jniLibs.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIVER_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
#WORKSPACE_DIR="$(cd "$DRIVER_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "/home/dev/BRNavi/" && pwd)"
CRATE_NAME="quic-jsonrpc-client"
LIB_NAME="libquic_jsonrpc_client.so"
OUTPUT_DIR="$DRIVER_DIR/android/app/src/main/jniLibs"
PROFILE="release"
DRY_RUN="false"
ABIS=("arm64-v8a")
INSTALL_MISSING_TARGETS="false"
SKIP_MISSING_TARGETS="false"

usage() {
  cat <<'EOF'
Usage: tool/build_rust_android.sh [options]

Options:
  --profile <debug|release>   Cargo profile (default: release)
  --abis <csv>                ABI list, e.g. arm64-v8a,armeabi-v7a,x86_64
  --install-missing-targets   Install missing rustup targets automatically
  --skip-missing-targets      Skip ABIs with missing rustup targets
  --dry-run                   Print commands without executing
  -h, --help                  Show this help

Requirements:
  - cargo
  - cargo-ndk (install: cargo install cargo-ndk)
  - Android NDK available in Android SDK
  - Workspace crate: quic-jsonrpc-client
EOF
}

log() {
  echo "[rust-android] $*"
}

run_cmd() {
  if [[ "$DRY_RUN" == "true" ]]; then
    echo "+ $*"
  else
    "$@"
  fi
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --profile)
        PROFILE="$2"
        shift 2
        ;;
      --abis)
        IFS=',' read -r -a ABIS <<< "$2"
        shift 2
        ;;
      --dry-run)
        DRY_RUN="true"
        shift
        ;;
      --install-missing-targets)
        INSTALL_MISSING_TARGETS="true"
        shift
        ;;
      --skip-missing-targets)
        SKIP_MISSING_TARGETS="true"
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        echo "Unknown argument: $1" >&2
        usage
        exit 1
        ;;
    esac
  done

  if [[ "$PROFILE" != "debug" && "$PROFILE" != "release" ]]; then
    echo "Invalid --profile: $PROFILE" >&2
    exit 1
  fi
}

ensure_tools() {
  command -v cargo >/dev/null 2>&1 || {
    echo "cargo is required" >&2
    exit 1
  }

  cargo ndk --help >/dev/null 2>&1 || {
    cat >&2 <<'EOF'
cargo-ndk is required. Install it with:
  cargo install cargo-ndk
EOF
    exit 1
  }

  command -v rustup >/dev/null 2>&1 || {
    echo "rustup is required" >&2
    exit 1
  }
}

abi_to_rust_target() {
  case "$1" in
    arm64-v8a)
      echo "aarch64-linux-android"
      ;;
    armeabi-v7a)
      echo "armv7-linux-androideabi"
      ;;
    x86_64)
      echo "x86_64-linux-android"
      ;;
    *)
      echo ""
      ;;
  esac
}

ensure_rust_target_for_abi() {
  local abi="$1"
  local target
  target="$(abi_to_rust_target "$abi")"

  if [[ -z "$target" ]]; then
    echo "Unsupported ABI: $abi" >&2
    return 2
  fi

  if rustup target list --installed | grep -qx "$target"; then
    return 0
  fi

  if [[ "$INSTALL_MISSING_TARGETS" == "true" ]]; then
    log "Installing missing rust target: $target"
    run_cmd rustup target add "$target"
    return 0
  fi

  if [[ "$SKIP_MISSING_TARGETS" == "true" ]]; then
    log "Skipping $abi (missing rust target $target)"
    return 1
  fi

  cat >&2 <<EOF
Missing rust target for ABI '$abi': $target
Install it with:
  rustup target add $target
Or rerun with one of:
  --install-missing-targets
  --skip-missing-targets
EOF
  return 2
}

build_one_abi() {
  local abi="$1"

  ensure_rust_target_for_abi "$abi"
  local target_status=$?
  if [[ "$target_status" -ne 0 ]]; then
    if [[ "$target_status" -eq 1 ]]; then
      return 10
    fi
    return 1
  fi

  local profile_flag=()
  if [[ "$PROFILE" == "release" ]]; then
    profile_flag=(--release)
  fi

  local cargo_cmd=(
    cargo ndk
    -t "$abi"
    -o "$OUTPUT_DIR"
    build
    -p "$CRATE_NAME"
    "${profile_flag[@]}"
  )

  log "Building $CRATE_NAME for ABI: $abi ($PROFILE)"
  run_cmd "${cargo_cmd[@]}"

  local so_path="$OUTPUT_DIR/$abi/$LIB_NAME"
  if [[ "$DRY_RUN" != "true" ]]; then
    if [[ ! -f "$so_path" ]]; then
      echo "Expected output not found: $so_path" >&2
      exit 1
    fi
    log "Output ready: $so_path"
  fi
}

main() {
  parse_args "$@"
  ensure_tools

  mkdir -p "$OUTPUT_DIR"
  cd "$WORKSPACE_DIR"

  log "Workspace: $WORKSPACE_DIR"
  log "JNI output: $OUTPUT_DIR"

  local built_count=0
  local skipped_count=0
  for abi in "${ABIS[@]}"; do
    if build_one_abi "$abi"; then
      built_count=$((built_count + 1))
      continue
    fi

    rc="$?"
    if [[ "$rc" -eq 10 ]]; then
      skipped_count=$((skipped_count + 1))
      continue
    fi
    exit "$rc"
  done

  if [[ "$built_count" -eq 0 ]]; then
    echo "No ABI was built successfully." >&2
    exit 1
  fi

  if [[ "$skipped_count" -gt 0 ]]; then
    log "Skipped ABIs: $skipped_count"
  fi

  log "Done"
}

main "$@"
