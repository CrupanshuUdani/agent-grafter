#!/usr/bin/env bash
# A release tag must name the exact version both crates carry, and
# agent-grafter must require that same safelink version: the two are
# published in lockstep, safelink first.
set -euo pipefail
tag="${1:?usage: check-version.sh <vX.Y.Z>}"
case "$tag" in
  v[0-9]*) version="${tag#v}" ;;
  *) echo "::error::tag '$tag' must look like v1.2.3"; exit 1 ;;
esac

meta="$(cargo metadata --format-version 1 --no-deps)"
for crate in safelink agent-grafter; do
  actual="$(jq -r --arg n "$crate" '.packages[] | select(.name == $n) | .version' <<<"$meta")"
  if [ "$actual" != "$version" ]; then
    echo "::error::$crate is at $actual but the tag says $version"
    exit 1
  fi
done
req="$(jq -r '.packages[] | select(.name == "agent-grafter") | .dependencies[] | select(.name == "safelink") | .req' <<<"$meta")"
if [ "$req" != "^$version" ]; then
  echo "::error::agent-grafter requires safelink $req, expected ^$version"
  exit 1
fi
echo "version $version matches both crates"
