# Repository settings

How the GitHub repository is locked down, and why. `scripts/apply-repo-settings.sh`
applies all of it (`--dry-run` shows what it would do); the rulesets are in
`.github/rulesets/`. The project has one maintainer, so the rules are set up to keep
`main` and releases safe from mistakes and outside changes, without blocking the
maintainer's own work.

## Branch `main` (ruleset `main`)

| Rule | Why |
|---|---|
| Changes only through pull requests | Every change is visible, reviewed and tested before it lands. |
| 1 approval, from the code owner (`.github/CODEOWNERS`) | Outside contributions need the maintainer's review. |
| New pushes dismiss earlier approvals | A PR cannot be changed after it was approved. |
| All review conversations resolved | Open questions are not merged by accident. |
| CI checks `linux` and `windows` must pass | Tests and the Windows build run on every PR. |
| Check `no-chat-links` must pass | No links to AI chat sessions in PR titles, descriptions, commit messages or files. |
| No force pushes, no deletion, linear history | History is never rewritten; squash merges only. |
| Bypass: repository admins, for pull requests only | The maintainer can merge their own PRs (nobody else can approve them), but cannot push to `main` directly. |

## Release tags (ruleset `release tags`)

Only repository admins can create, move or delete `v*` tags. Pushing a tag publishes a
release (`.github/workflows/release.yml`), so this is what controls releases.

## Other settings

| Setting | Value | Why |
|---|---|---|
| Merge methods | Squash only; delete branch after merge; no auto-merge | One commit per change, a tidy history. |
| Actions token | Read-only by default; workflows cannot approve PRs | Only `release.yml` asks for write access, to publish releases. |
| Outside contributors' PRs | CI waits for the maintainer's approval | A PR from a fork cannot run code on GitHub's runners unseen. |
| Private vulnerability reporting | On | See [SECURITY.md](../SECURITY.md). |
| Dependabot | Alerts, security fixes, and monthly update PRs (`.github/dependabot.yml`) | Known-vulnerable crates are flagged and patched. |
| Secret scanning and push protection | On (free for public repositories, but off until enabled) | A token committed by mistake is blocked or flagged. |
| Wiki, Projects | Off | Documentation lives in the repository; issues are the to-do list. |
| Labels | `bug`, `analysis`, `enhancement`, `game-profile`, `needs-info`, `good first issue` | Used by the issue templates and CONTRIBUTING.md. |

## Limits of the free plan

While the repository is **private on the free plan**, GitHub does not enforce rulesets
(branch and tag protection). The script skips them with a message; everything else
applies. They take effect once the repository is public (or on a paid plan).

## Before making the repository public

1. Run `scripts/apply-repo-settings.sh` once more right after switching to public, so the
   rulesets are created.
2. Check the Settings > Rules page shows both rulesets as active, and that a test PR cannot be
   merged without CI.
3. In Settings > Code security, confirm secret scanning and push protection are on.
4. Check the history has nothing private: `git log -p | grep -iE "password|token|secret|@"`,
   and no personal recordings or paths were ever committed.
5. Optionally, if GitHub offers it for the repository: make releases immutable (Settings >
   General, "Releases"), so files of a published release can never be swapped.
6. Commits show the author's e-mail address. To keep a private address out of future
   commits, set GitHub's no-reply address (Settings > Emails) as `git config user.email`.
