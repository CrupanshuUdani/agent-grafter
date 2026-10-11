#!/usr/bin/env bash
# Print the body of one CHANGELOG.md section: `## [<version>]` up to the
# next section or the link definitions at the bottom.
set -euo pipefail
version="${1:?usage: release-notes.sh <version|Unreleased> [changelog]}"
changelog="${2:-CHANGELOG.md}"

body="$(awk -v heading="## [$version]" '
  index($0, heading) == 1 { found = 1; next }
  found && (/^## \[/ || /^\[[^]]+\]: /) { exit }
  found { print }
  END { if (!found) exit 3 }
' "$changelog")" || exit $?

if [ -z "$(tr -d '[:space:]' <<<"$body")" ]; then
  echo "CHANGELOG section [$version] is empty" >&2
  exit 4
fi
# Trim leading and trailing blank lines.
sed -e '/./,$!d' <<<"$body" | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}'
