#!/bin/sh
set -eu

version="${1:?usage: install-leanctx.sh <version> [target-arch]}"
target_arch="${2:-}"

if [ -z "$target_arch" ]; then
  case "$(uname -m)" in
    x86_64) target_arch="amd64" ;;
    aarch64|arm64) target_arch="arm64" ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
  esac
fi

case "$target_arch" in
  amd64) target="x86_64-unknown-linux-gnu" ;;
  arm64) target="aarch64-unknown-linux-gnu" ;;
  *) echo "unsupported TARGETARCH=$target_arch" >&2; exit 1 ;;
esac

asset="lean-ctx-${target}.tar.gz"
base="https://github.com/yvgude/lean-ctx/releases/download/v${version}"
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

curl -fsSL -o "$workdir/SHA256SUMS" "$base/SHA256SUMS"
curl -fsSL -o "$workdir/$asset" "$base/$asset"

(
  cd "$workdir"
  grep "  $asset$" SHA256SUMS | sha256sum -c -
)

mkdir -p "$workdir/extract"
tar -xzf "$workdir/$asset" -C "$workdir/extract"

bin="$(find "$workdir/extract" -type f -name lean-ctx | head -n 1)"
test -n "$bin"

cp "$bin" /usr/local/bin/lean-ctx
chmod 0755 /usr/local/bin/lean-ctx
/usr/local/bin/lean-ctx --version
