#!/usr/bin/env bash
set -euo pipefail

# Build QUIC native .so and run Flutter app on Android.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIVER_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_SCRIPT="$SCRIPT_DIR/build_rust_android.sh"
CHECK_SCRIPT="$SCRIPT_DIR/check_quic_so.sh"

PROFILE="release"
ABIS="arm64-v8a"
DEVICE_ID=""
SKIP_BUILD="false"
SKIP_CHECK="false"
AUTO_ABI="false"
REBUILD_ON_MISSING="true"
FLUTTER_ARGS=()

usage() {
  cat <<'EOF'
Usage: tool/run_android_with_quic.sh [options] [-- <flutter run args>]

Options:
  --profile <debug|release>   Rust profile for .so build (default: release)
  --abis <csv>                Android ABIs for .so build (default: arm64-v8a)
  --device <id>               Pass -d <id> to flutter run
  --auto-abi                  Detect ABI from connected Android device via adb
  --skip-build                Skip .so build step
  --skip-check                Skip .so existence check
  --no-rebuild-on-missing     Disable auto rebuild when .so check fails
  -h, --help                  Show this help

Examples:
  ./tool/run_android_with_quic.sh
  ./tool/run_android_with_quic.sh --profile debug --abis arm64-v8a,x86_64
  ./tool/run_android_with_quic.sh --device R58W12345AB -- --verbose
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --profile)
        PROFILE="$2"
        shift 2
        ;;
      --abis)
        ABIS="$2"
        shift 2
        ;;
      --device)
        DEVICE_ID="$2"
        shift 2
        ;;
      --auto-abi)
        AUTO_ABI="true"
        shift
        ;;
      --skip-build)
        SKIP_BUILD="true"
        shift
        ;;
      --skip-check)
        SKIP_CHECK="true"
        shift
        ;;
      --no-rebuild-on-missing)
        REBUILD_ON_MISSING="false"
        shift
        ;;
      --)
        shift
        FLUTTER_ARGS=("$@")
        break
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        FLUTTER_ARGS+=("$1")
        shift
        ;;
    esac
  done

  if [[ "$PROFILE" != "debug" && "$PROFILE" != "release" ]]; then
    echo "Invalid --profile: $PROFILE" >&2
    exit 1
  fi
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

main() {
  parse_args "$@"

  if [[ "$AUTO_ABI" == "true" ]]; then
    ABIS="$(detect_device_abi)"
    echo "Detected device ABI: $ABIS"
  fi

  if [[ "$SKIP_BUILD" != "true" ]]; then
    "$BUILD_SCRIPT" --profile "$PROFILE" --abis "$ABIS"
  fi

  if [[ "$SKIP_CHECK" != "true" ]]; then
    check_cmd=("$CHECK_SCRIPT" --abis "$ABIS")
    if [[ "$AUTO_ABI" == "true" ]]; then
      check_cmd=("$CHECK_SCRIPT" --auto-abi)
      if [[ -n "$DEVICE_ID" ]]; then
        check_cmd+=(--device "$DEVICE_ID")
      fi
    fi

    if ! "${check_cmd[@]}"; then
      if [[ "$REBUILD_ON_MISSING" != "true" ]]; then
        exit 1
      fi

      rebuild_abis="$ABIS"
      if [[ "$AUTO_ABI" == "true" ]]; then
        rebuild_abis="$(detect_device_abi)"
      fi

      echo "Missing QUIC .so detected. Rebuilding ABI(s): $rebuild_abis"
      "$BUILD_SCRIPT" --profile "$PROFILE" --abis "$rebuild_abis"

      # Validate again after rebuild.
      check_cmd=("$CHECK_SCRIPT" --abis "$rebuild_abis")
      if [[ "$AUTO_ABI" == "true" ]]; then
        check_cmd=("$CHECK_SCRIPT" --auto-abi)
        if [[ -n "$DEVICE_ID" ]]; then
          check_cmd+=(--device "$DEVICE_ID")
        fi
      fi
      "${check_cmd[@]}"
    fi
  fi

  cd "$DRIVER_DIR"

  run_cmd=(flutter run)
  if [[ -n "$DEVICE_ID" ]]; then
    run_cmd+=(-d "$DEVICE_ID")
  fi
  run_cmd+=("${FLUTTER_ARGS[@]}")

  echo "+ ${run_cmd[*]}"
  "${run_cmd[@]}"
}

main "$@"
