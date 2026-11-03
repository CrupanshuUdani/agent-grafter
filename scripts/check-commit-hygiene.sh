#!/usr/bin/env bash
# Fail when a commit message or an added line points at the process that
# produced the change (task or ruling numbers, tool attribution trailers)
# instead of describing the change itself.
set -euo pipefail
base="${1:?usage: check-commit-hygiene.sh <base> [head]}"
head="${2:-HEAD}"

pattern='Claude-Session|Co-Authored-By:.*(Claude|Anthropic)|Generated with .*Claude|claude\.ai/code|Ruling [0-9]+|Task ([0-9]+|S[0-9]+)|Global Constraint|[Ff]ix round [0-9]'

bad=0
for sha in $(git rev-list --no-merges "$base..$head"); do
  hits="$(git log -1 --format=%B "$sha" | grep -nE "$pattern" || true)"
  if [ -n "$hits" ]; then
    echo "::error::commit $(git rev-parse --short "$sha") message refers to process, not the change:"
    echo "$hits"
    bad=1
  fi
done

# This script and its tests name the patterns on purpose.
hits="$(git diff -U0 "$base...$head" -- . ':!scripts/check-commit-hygiene.sh' ':!scripts/test-release-scripts.sh' \
  | grep -E '^\+[^+]' | grep -nE "$pattern" || true)"
if [ -n "$hits" ]; then
  echo "::error::added lines refer to process, not the change:"
  echo "$hits"
  bad=1
fi

[ "$bad" -eq 0 ] && echo "commit hygiene: clean"
exit "$bad"
