#!/usr/bin/env bash
set -euo pipefail

# Check that QUIC native shared libraries exist for requested Android ABIs.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIVER_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
JNI_DIR="$DRIVER_DIR/android/app/src/main/jniLibs"
LIB_NAME="libquic_jsonrpc_client.so"
ABIS=("arm64-v8a")
AUTO_ABI="false"
DEVICE_ID=""

usage() {
  cat <<'EOF'
Usage: tool/check_quic_so.sh [options]

Options:
  --abis <csv>      ABI list to validate (default: arm64-v8a)
  --auto-abi        Detect ABI from connected Android device via adb
  --device <id>     Device serial for adb (used with --auto-abi)
  -h, --help        Show this help

Examples:
  ./tool/check_quic_so.sh
  ./tool/check_quic_so.sh --abis arm64-v8a,armeabi-v7a,x86_64
  ./tool/check_quic_so.sh --auto-abi --device R58W12345AB
EOF
}

detect_device_abi() {
  command -v adb >/dev/null 2>&1 || {
    echo "adb is required for --auto-abi" >&2
    exit 1
  }

  local adb_cmd=(adb)
  if [[ -n "$DEVICE_ID" ]]; then
    adb_cmd+=(-s "$DEVICE_ID")
  fi

  local abi_list
  abi_list="$(${adb_cmd[@]} shell getprop ro.product.cpu.abilist 2>/dev/null | tr -d '\r')"
  if [[ -z "$abi_list" ]]; then
    abi_list="$(${adb_cmd[@]} shell getprop ro.product.cpu.abi 2>/dev/null | tr -d '\r')"
  fi

  if [[ -z "$abi_list" ]]; then
    echo "Failed to detect device ABI via adb" >&2
    exit 1
  fi

  IFS=',' read -r -a device_abis <<< "$abi_list"
  for abi in "${device_abis[@]}"; do
    case "$abi" in
      arm64-v8a|armeabi-v7a|x86_64)
        echo "$abi"
        return 0
        ;;
    esac
  done

  echo "No supported ABI found in device list: $abi_list" >&2
  exit 1
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --abis)
        IFS=',' read -r -a ABIS <<< "$2"
        shift 2
        ;;
      --auto-abi)
        AUTO_ABI="true"
        shift
        ;;
      --device)
        DEVICE_ID="$2"
        shift 2
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
}

main() {
  parse_args "$@"

  if [[ "$AUTO_ABI" == "true" ]]; then
    detected_abi="$(detect_device_abi)"
    ABIS=("$detected_abi")
    echo "Detected device ABI: $detected_abi"
  fi

  local missing=0
  for abi in "${ABIS[@]}"; do
    local so_path="$JNI_DIR/$abi/$LIB_NAME"
    if [[ -f "$so_path" ]]; then
      echo "[ok] $so_path"
    else
      echo "[missing] $so_path" >&2
      missing=1
    fi
  done

  if [[ "$missing" -ne 0 ]]; then
    echo "One or more QUIC .so files are missing." >&2
    exit 1
  fi

  echo "All requested QUIC .so files are present."
}

main "$@"
