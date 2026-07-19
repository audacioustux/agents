#!/bin/sh
set -eu

version="${1:?usage: setup-image.sh <leanctx-version> [target-arch]}"
target_arch="${2:-}"

apt-get update
apt-get install -y --no-install-recommends ca-certificates curl tar findutils git
rm -rf /var/lib/apt/lists/*

install-leanctx "$version" "$target_arch"

useradd --system --uid 10001 --create-home --home-dir /var/lib/lean-ctx leanctx
