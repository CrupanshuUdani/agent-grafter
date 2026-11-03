#!/usr/bin/env bash
# Fail when a commit message or an added line points at the process that
# produced the change (task or ruling numbers, AI tool attribution) instead
# of describing the change itself.
set -euo pipefail
base="${1:?usage: check-commit-hygiene.sh <base> [head]}"
head="${2:-HEAD}"

# Case-sensitive: numbered references into a plan or task list.
process='Ruling [0-9]+|Task ([0-9]+|S[0-9]+)|Global Constraint|[Ff]ix round [0-9]'

# Case-insensitive: attribution to an AI tool. Only attribution phrasing is
# matched, so a document that merely names a tool, or a person who shares a
# tool's name, is not flagged.
agents='Claude|Anthropic|Codex|OpenAI|ChatGPT|Copilot|Cursor|Gemini|Aider|Windsurf|Cline|Kiro|Junie'
attribution="Claude-Session|claude\.ai/code|chatgpt\.com/codex|cursor\.com/(agents|background)"
attribution="$attribution|Co-Authored-By:.*($agents)"
attribution="$attribution|(Assisted|Generated)-By:"
attribution="$attribution|(Generated|Created|Written|Authored|Assisted) (with|by) .*($agents)"

scan() {
  local text
  text="$(cat)"
  { grep -nE "$process" <<<"$text" || true; grep -niE "$attribution" <<<"$text" || true; } | sort -t: -k1,1n -u
}

bad=0
for sha in $(git rev-list --no-merges "$base..$head"); do
  hits="$(git log -1 --format=%B "$sha" | scan)"
  if [ -n "$hits" ]; then
    echo "::error::commit $(git rev-parse --short "$sha") message refers to process, not the change:"
    echo "$hits"
    bad=1
  fi
done

# This script and its tests name the patterns on purpose.
hits="$(git diff -U0 "$base...$head" -- . ':!scripts/check-commit-hygiene.sh' ':!scripts/test-release-scripts.sh' \
  | { grep -E '^\+[^+]' || true; } | scan)"
if [ -n "$hits" ]; then
  echo "::error::added lines refer to process, not the change:"
  echo "$hits"
  bad=1
fi

[ "$bad" -eq 0 ] && echo "commit hygiene: clean"
exit "$bad"
