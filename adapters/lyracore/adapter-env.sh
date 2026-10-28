#!/usr/bin/env bash

ADAPTER_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
HARNESS_DIR=$(cd "$ADAPTER_DIR/../.." && pwd)

_is_lyracore() {
  [ -d "$1/native-host" ] && [ -d "$1/module" ] && [ -f "$1/Cargo.toml" ]
}

resolve_lyracore() {
  local given=${LYRACORE_DIR:-$PWD}
  LYRACORE_DIR=$(cd "$given" 2>/dev/null && pwd) || {
    echo "[adapter] LyraCore checkout does not exist: $given" >&2
    return 2
  }
  if ! _is_lyracore "$LYRACORE_DIR"; then
    echo "[adapter] set LYRACORE_DIR to a checkout with native-host/, module/ and Cargo.toml" >&2
    return 2
  fi
  export LYRACORE_DIR
}

wire_build() {
  if [ -n "${WIRE_BIN:-}" ]; then
    [ -x "$WIRE_BIN" ] || { echo "[adapter] WIRE_BIN is not executable: $WIRE_BIN" >&2; return 2; }
    return 0
  fi
  cargo build --quiet --locked --manifest-path "$HARNESS_DIR/Cargo.toml" --bin vanilla-wire || return
  WIRE_BIN=${CARGO_TARGET_DIR:-$HARNESS_DIR/target}/debug/vanilla-wire
}
