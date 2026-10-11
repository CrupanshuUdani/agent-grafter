#!/usr/bin/env bash
# Tests for the release helper scripts. Run from the repo root.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

cat > "$tmp/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

### Added
- next thing

## [0.2.0] - 2026-10-01

### Fixed
- a fix

## [0.1.0] - 2026-09-30

## [0.0.9] - 2026-09-01

- old

[0.2.0]: https://example.invalid/compare/v0.1.0...v0.2.0
EOF

# release-notes.sh
out="$(bash "$here/release-notes.sh" 0.2.0 "$tmp/CHANGELOG.md")"
grep -q -- "- a fix" <<<"$out" || fail "0.2.0 body missing"
grep -q "next thing" <<<"$out" && fail "0.2.0 leaked the Unreleased section"
grep -q "0.1.0" <<<"$out" && fail "0.2.0 ran into the next section"
grep -q "example.invalid" <<<"$out" && fail "0.2.0 included link definitions"
out="$(bash "$here/release-notes.sh" Unreleased "$tmp/CHANGELOG.md")"
grep -q "next thing" <<<"$out" || fail "Unreleased body missing"
set +e
bash "$here/release-notes.sh" 9.9.9 "$tmp/CHANGELOG.md" >/dev/null 2>&1; rc=$?
[ "$rc" -eq 3 ] || fail "missing version should exit 3, got $rc"
bash "$here/release-notes.sh" 0.1.0 "$tmp/CHANGELOG.md" >/dev/null 2>&1; rc=$?
[ "$rc" -eq 4 ] || fail "empty section should exit 4, got $rc"
# A dot in the version must not match any character.
bash "$here/release-notes.sh" 0x2x0 "$tmp/CHANGELOG.md" >/dev/null 2>&1; rc=$?
[ "$rc" -eq 3 ] || fail "version must match literally, got $rc"

# check-version.sh (against this repo's real manifests)
version="$(cargo metadata --format-version 1 --no-deps | jq -r '.packages[] | select(.name=="agent-grafter") | .version')"
bash "$here/check-version.sh" "v$version" >/dev/null 2>&1; rc=$?
[ "$rc" -eq 0 ] || fail "v$version should pass, got $rc"
bash "$here/check-version.sh" "v9.9.9" >/dev/null 2>&1; rc=$?
[ "$rc" -eq 1 ] || fail "v9.9.9 should fail, got $rc"
bash "$here/check-version.sh" "$version" >/dev/null 2>&1; rc=$?
[ "$rc" -eq 1 ] || fail "a tag without the v prefix should fail, got $rc"
set -e

# release-package.sh
mkdir -p "$tmp/extras" "$tmp/out"
for f in LICENSE NOTICE README.md THIRD-PARTY-LICENSES; do echo "$f" > "$tmp/extras/$f"; done
printf '#!/bin/sh\n' > "$tmp/agent-grafter"
archive="$(bash "$here/release-package.sh" 1.2.3 x86_64-unknown-linux-musl "$tmp/agent-grafter" "$tmp/out" "$tmp/extras")"
[ "$archive" = "$tmp/out/agent-grafter-v1.2.3-x86_64-unknown-linux-musl.tar.gz" ] || fail "unexpected archive path $archive"
listing="$(tar -tzf "$archive" | sort)"
for f in agent-grafter LICENSE NOTICE README.md THIRD-PARTY-LICENSES; do
  grep -qx "agent-grafter-v1.2.3-x86_64-unknown-linux-musl/$f" <<<"$listing" || fail "tar missing $f"
done
tar -xzf "$archive" -C "$tmp/out"
[ -x "$tmp/out/agent-grafter-v1.2.3-x86_64-unknown-linux-musl/agent-grafter" ] || fail "binary lost its executable bit"
printf 'MZ' > "$tmp/agent-grafter.exe"
archive="$(bash "$here/release-package.sh" 1.2.3 x86_64-pc-windows-msvc "$tmp/agent-grafter.exe" "$tmp/out" "$tmp/extras")"
[ "$archive" = "$tmp/out/agent-grafter-v1.2.3-x86_64-pc-windows-msvc.zip" ] || fail "unexpected zip path $archive"
unzip -l "$archive" | grep -q "agent-grafter-v1.2.3-x86_64-pc-windows-msvc/agent-grafter.exe" || fail "zip missing exe"

echo "release script tests: all passed"
