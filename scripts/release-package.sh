#!/usr/bin/env bash
# Package one release binary with the files its licence obliges us to ship.
set -euo pipefail
version="${1:?usage: release-package.sh <version> <target> <binary> <out-dir> <extras-dir>}"
target="${2:?}"; binary="${3:?}"; out="${4:?}"; extras="${5:?}"

name="agent-grafter-v${version}-${target}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name" "$out"
cp "$binary" "$stage/$name/"
for f in LICENSE NOTICE README.md THIRD-PARTY-LICENSES; do
  cp "$extras/$f" "$stage/$name/"
done

out="$(cd "$out" && pwd)"
case "$target" in
  *windows*)
    archive="$out/$name.zip"
    (cd "$stage" && 7z a -tzip -bso0 -bsp0 "$archive" "$name")
    ;;
  *)
    chmod +x "$stage/$name/$(basename "$binary")"
    archive="$out/$name.tar.gz"
    tar -czf "$archive" -C "$stage" "$name"
    ;;
esac
echo "$archive"
