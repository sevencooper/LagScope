#!/usr/bin/env bash
# Applies the repository settings described in docs/REPO_SETTINGS.md with the GitHub CLI.
# Safe to run again: rulesets are updated in place, labels are overwritten.
#   scripts/apply-repo-settings.sh            # apply
#   scripts/apply-repo-settings.sh --dry-run  # only show what would be done
# Branch and tag rulesets need a public repository (or a paid plan for private ones).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="${REPO:-sevencooper/LagScope}"
DRY=0; [[ "${1:-}" == "--dry-run" ]] && DRY=1
run() { echo "+ $*"; [[ $DRY == 1 ]] || "$@"; }
# For settings GitHub may refuse on some plans / repo types: warn and carry on.
try() { run "$@" || echo "  ! not applied - set it by hand (see docs/REPO_SETTINGS.md)"; }

# Features and merging: squash only, branches deleted after merge.
run gh repo edit "$REPO" --enable-issues --enable-wiki=false --enable-projects=false \
  --enable-squash-merge --enable-merge-commit=false --enable-rebase-merge=false \
  --delete-branch-on-merge --enable-auto-merge=false

# Rulesets: create, or update the one with the same name.
rulesets=1
existing=$(gh api "repos/$REPO/rulesets" --jq '.[] | "\(.id) \(.name)"' 2>/dev/null) || rulesets=0
if [[ $rulesets == 0 ]]; then
  echo "! rulesets are not available for this repository (private on the free plan): skipped"
fi
for f in .github/rulesets/*.json; do
  [[ $rulesets == 1 ]] || break
  name=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['name'])" "$f")
  id=$(awk -v n="$name" '{ i = $1; $1 = ""; if (substr($0, 2) == n) print i }' <<< "$existing")
  if [[ -n "$id" ]]; then
    try gh api -X PUT "repos/$REPO/rulesets/$id" --input "$f" --silent
  else
    try gh api -X POST "repos/$REPO/rulesets" --input "$f" --silent
  fi
done

# Actions: read-only token by default (release.yml asks for write itself), workflows may not
# approve PRs, and pull requests from outside contributors wait for approval before CI runs.
run gh api -X PUT "repos/$REPO/actions/permissions/workflow" \
  -f default_workflow_permissions=read -F can_approve_pull_request_reviews=false --silent
try gh api -X PUT "repos/$REPO/actions/permissions/fork-pr-contributor-approval" \
  -f approval_policy=all_external_contributors --silent

# Security: private vulnerability reports, Dependabot alerts and security fixes.
try gh api -X PUT "repos/$REPO/private-vulnerability-reporting" --silent
try gh api -X PUT "repos/$REPO/vulnerability-alerts" --silent
try gh api -X PUT "repos/$REPO/automated-security-fixes" --silent
# Secret scanning with push protection (free for public repositories, but off by default).
try gh api -X PATCH "repos/$REPO" --silent --input - <<'JSON'
{"security_and_analysis": {"secret_scanning": {"status": "enabled"}, "secret_scanning_push_protection": {"status": "enabled"}}}
JSON

# Labels used by the issue templates and CONTRIBUTING.md.
while IFS='|' read -r name color desc; do
  run gh label create "$name" --repo "$REPO" --color "$color" --description "$desc" --force
done <<'LABELS'
bug|d73a4a|LagScope crashed, failed or misbehaves
analysis|fbca04|The diagnosis was wrong or unclear
enhancement|a2eeef|New feature or improvement
game-profile|5319e7|Support for a game
needs-info|cfd3d7|Waiting for details or a recording
good first issue|7057ff|Good for newcomers
LABELS
echo "done"
