#!/usr/bin/env bash
# Plain install: build with cargo, copy the rushi binary to $PREFIX/bin.
# Deferred path for non-Nix users. The Nix flake (flake.nix) is the
# primary install path for the author's flake-managed machines.
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
PROFILE="${RUSHI_PROFILE:-release}"

# `debug` is an alias for cargo's built-in `dev` profile.
if [ "$PROFILE" = "debug" ]; then
  PROFILE="dev"
fi

echo "rushi: building ($PROFILE)…"
if [ "$PROFILE" = "dev" ]; then
  cargo build
  OUT_DIR="target/debug"
else
  cargo build --profile "$PROFILE"
  OUT_DIR="target/$PROFILE"
fi

mkdir -p "$PREFIX/bin"
cp "$OUT_DIR/rushi" "$PREFIX/bin/rushi"

echo "rushi: installed to $PREFIX/bin/rushi"
echo "rushi: add $PREFIX/bin to your PATH if not already present."
