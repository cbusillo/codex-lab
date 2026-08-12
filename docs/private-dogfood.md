# Codex Lab private dogfood

This guide installs the macOS ARM64 Codex Lab prerelease without a Git checkout
or source build. The release is public on GitHub, but it is intended only for
the bounded private-dogfood group.

## Prerequisites

- An Apple Silicon Mac running macOS 13 or newer. Confirm `uname -m` prints
  `arm64`, and use `sw_vers -productVersion` to record the macOS version.
- Python 3.10 or newer, plus `curl` and `tar`. Confirm with
  `python3 --version`; install a current Python from python.org or Homebrew if
  `python3` is missing or older. A stock macOS install does not guarantee a
  compatible Python runtime.
- The official ChatGPT app installed in `/Applications` or `~/Applications`.
  Codex Lab accepts only the unmodified app signed by OpenAI team `2DC432GLL2`;
  it does not patch or redistribute that app.
- Quit the official ChatGPT app before launching `Codex Lab.app`. The Lab
  launcher must start a fresh app process with the pinned loopback app-server
  connection.

## Install the exact prerelease

Paste this helper into Terminal. It downloads the repository source archive for
one exact immutable commit only to obtain the standard-library installer,
deletes the archive and extracted files when the command finishes, and does not
invoke Git or Cargo. The installer commit below is the reviewed release-gate
repair and is included unchanged in the release candidate.

```sh
run_codex_lab_installer() (
  set -eu
  installer_commit="$1"
  shift
  installer_dir="$(mktemp -d "${TMPDIR:-/tmp}/codex-lab-installer.XXXXXX")"
  trap 'rm -rf "$installer_dir"' EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    "https://github.com/cbusillo/codex-lab/archive/${installer_commit}.tar.gz" \
    --output "$installer_dir/source.tar.gz"
  tar -xzf "$installer_dir/source.tar.gz" \
    --strip-components=1 \
    -C "$installer_dir"
  python3 "$installer_dir/scripts/install_codex_lab.py" "$@"
)

verify_codex_lab_provenance() (
  expected_release_sha="$1"
  expected_app_path="${2:-$HOME/Applications/Codex Lab.app}"
  expected_version="${3:-0.1.0}"
  codex-lab debug provenance --json | \
    EXPECTED_RELEASE_SHA="$expected_release_sha" \
    EXPECTED_APP_PATH="$expected_app_path" \
    EXPECTED_VERSION="$expected_version" python3 -c '
import json
import os
import re
import sys
from pathlib import Path

expected = os.environ["EXPECTED_RELEASE_SHA"]
if re.fullmatch(r"[0-9a-f]{40}", expected) is None:
    raise SystemExit("expected release SHA is not 40 lowercase hexadecimal characters")
provenance = json.load(sys.stdin)
expected_fields = {
    "version": os.environ["EXPECTED_VERSION"],
    "source_commit": expected,
    "dirty_state": "clean",
    "build_profile": "release",
    "build_channel": "lab",
}
for field, value in expected_fields.items():
    if provenance.get(field) != value:
        raise SystemExit(f"installed {field} does not match: {provenance.get(field)!r}")
app = Path(os.environ["EXPECTED_APP_PATH"]).resolve()
executable = Path(provenance.get("executable_path", "")).resolve()
if app not in executable.parents:
    raise SystemExit("installed executable_path is outside Codex Lab.app")
print("installed provenance matches the release contract")
'
)
```

Re-paste both helper functions in each new Terminal session before using later
status, rollback, or reinstall command blocks.

The helper deliberately removes the downloaded installer source after every
command, so each later `--status`, rollback, or uninstall invocation downloads
the same immutable archive again and requires network access.

Install the pinned candidate:

```sh
installer_commit=902f6ddd7a797ce432c6e635552b854dd53ce00a
release_tag=codex-lab-v0.1.0-lab.4
run_codex_lab_installer "$installer_commit" \
  --release-tag "$release_tag" \
  --force
```

The installer verifies the release URLs, asset sizes, SHA-256 checksums,
archive layout, exact source provenance, Developer ID identity and team,
hardened runtime, and required V8 entitlements before replacing anything. It
then installs the app, shim, signed CLI and Code Mode host, state record, and
launchd supervisor as a rollback-aware transaction. The normal locations are:

- `~/Applications/Codex Lab.app`
- `~/.local/bin/codex-lab`
- `~/Library/Application Support/Codex Lab/install-state.json`
- `dev.everycode.codex-lab.app-server.v1` in the user launchd domain

Add `~/.local/bin` to `PATH` if Terminal cannot find `codex-lab`.

## Verify the installed release

Run all of these before dogfooding:

```sh
installer_commit=902f6ddd7a797ce432c6e635552b854dd53ce00a
run_codex_lab_installer "$installer_commit" --status
codex-lab --version
codex-lab debug provenance --json
codex-lab doctor
launchctl print "gui/$(id -u)/dev.everycode.codex-lab.app-server.v1"
```

The installer status must name `codex-lab-v0.1.0-lab.4` and bundle version
`41`. Structured provenance must report `dirty_state` as `clean`,
`build_profile` as `release`, `build_channel` as `lab`, and an
`executable_path` inside the installed `Codex Lab.app`.

Known version-surface limitation
[#515](https://github.com/cbusillo/codex-lab/issues/515): `codex-lab exec` may
print `OpenAI Codex v0.0.0` in its startup banner. Treat that banner as a known
display bug only when `codex-lab --version` reports version `0.1.0`, installer
status names the expected release tag, and structured provenance matches the
release contract. The semantic version is shared by `lab.3` and `lab.4`; only
installer status and the provenance `source_commit` distinguish those releases.
Stop and report a release blocker if any authoritative tag or provenance check
disagrees.

The release handoff must supply the exact 40-character release SHA next to this
guide. A committed guide cannot name the future merge commit that contains
itself. Refuse the canary if the handoff omits that SHA. Set it below, then make
the comparison mechanical with the helper above:

```sh
expected_release_sha=PASTE_THE_40_CHARACTER_RELEASE_SHA_FROM_THE_HANDOFF
verify_codex_lab_provenance "$expected_release_sha"
```

The same SHA must appear in the published release manifest and GitHub release
target. The launchd record must show the managed engine and loopback listener
`ws://127.0.0.1:4766/rpc`.

Launch the TUI with `codex-lab`. Use `/status` for the current model,
permissions, workspace, and token context; `/usage` for account limits;
`/model` for model and reasoning effort; `/settings` for accounts, automatic
validation, and third-party agents; `/agent` for agent threads; and `/ps` for
background terminals.

To launch the official Mac controller through Codex Lab, first quit any running
official ChatGPT app, then open:

```sh
open "$HOME/Applications/Codex Lab.app"
```

The Lab launcher fails closed instead of silently falling back to the official
app's bundled stdio app-server.

## Authentication and accounts

The installer does not delete or replace Codex Lab authentication state under
`~/.codex-lab`. On first launch, sign in with ChatGPT when prompted. In the TUI,
`/login` opens Connected Accounts, `/login add` adds another ChatGPT account or
API key, and selecting a connected account makes it active. `/settings` controls
whether Codex automatically switches to another connected account after a
rate or usage limit and whether saved API keys are fallback-only.

After signing in, `codex-lab login status` must confirm authentication without
printing or sharing the underlying credential.

Known account-pool limitation
[#519](https://github.com/cbusillo/codex-lab/issues/519): an inactive account
with terminally expired, reused, or revoked refresh credentials can currently
interfere with a healthy active account when automatic rate-limit switching is
enabled. Before the canary, inspect Connected Accounts and repair any account
that already requires reauthentication. Apply the mitigation below if startup
says the access token could not be refreshed because the refresh token has
expired, was already used, or was revoked, followed by “Please log out and sign
in again.” Internal diagnostics may identify those terminal failures as
`refresh_token_expired`, `refresh_token_reused`, or
`refresh_token_invalidated`. Keep the healthy account selected and disable
automatic account switching in `/settings`, or launch the bounded CLI canary
with:

```sh
codex-lab -c auto_switch_accounts_on_rate_limit=false
```

This workaround does not delete stored accounts and does not prevent manual
account selection. The `-c` override applies only to that process; disabling the
setting through `/settings` persists across the restart/resume step. Record
which mitigation was used and treat automatic switching as a known limitation;
do not repeatedly retry a terminally invalid account. A generic model-API 401,
`token_expired`, or an ambiguous network error is not by itself proof that an
account requires reauthentication.

Never paste tokens, API keys, account email addresses, usage details, or the
contents of `auth.json` into an issue or dogfood report.

## Permissions and providers

Use `/permissions` to inspect or change what Codex may do. The
`danger-full-access` sandbox and `never` approval policy (no approval prompts)
can let the model run commands and change files without another prompt. The
`--dangerously-bypass-approvals-and-sandbox` option (YOLO mode) removes both
controls and is appropriate only inside an external sandbox whose recovery
boundary you understand. For the canary, use the least privilege compatible
with the participant's selected real task, start from a clean worktree or other
understood recovery point, and review the resulting diff before keeping changes.

Use `/model` for the primary Codex model and `/settings` for third-party-agent
configuration. One external-agent selector limitation remains relevant to the
canary:

- [#581](https://github.com/cbusillo/codex-lab/issues/581): some discovered
  Antigravity/Gemini selectors can be advertised in a malformed form and fail
  before the provider starts.

The built-in Antigravity read-only selector runs AGY in `plan` mode with AGY's
own sandbox enabled. Tool confirmations are approved noninteractively only
inside that sandbox, allowing repository inspection commands without granting a
write-capable agent session. If AGY still denies a required tool, Codex Lab
reports `permission_denied` with the bounded provider diagnostic instead of a
completed empty result.

A preflight failure, permission denial, or completed zero-line result is not
external-agent evidence. Report the exact selector and bounded diagnostic
without credentials, and use another already configured valid selector for the
canary. Do not grant unrestricted write access to obtain a review result.

## Restart and resume

Exit the TUI, restart the signed supervisor, and resume the most recent thread:

```sh
launchctl kickstart -k "gui/$(id -u)/dev.everycode.codex-lab.app-server.v1"
codex-lab resume --last
```

For the Mac controller, quit the official ChatGPT app before reopening
`~/Applications/Codex Lab.app` so it reconnects through the Lab launcher.

## Roll back and optionally reinstall

Roll back through the same transactional installer path to the exact supported
prior release:

```sh
installer_commit=902f6ddd7a797ce432c6e635552b854dd53ce00a
rollback_tag=codex-lab-v0.1.0-lab.3
run_codex_lab_installer "$installer_commit" \
  --release-tag "$rollback_tag" \
  --force
run_codex_lab_installer "$installer_commit" --status
verify_codex_lab_provenance c7f8a50f1564371e4472d00b685189697bf30c7c
```

Reinstall the candidate with the install command above. After either change,
confirm the supervisor is healthy and that status and provenance agree. After
rollback, status must name `codex-lab-v0.1.0-lab.3` and provenance
`source_commit` must equal `c7f8a50f1564371e4472d00b685189697bf30c7c`.
A failure must leave the prior app, shim, engine binaries, installer state, and
supervisor usable; if it does not, stop and report a release blocker.

## Bounded second-user canary

Use one genuine, bounded task the participant already needs to complete in a
real repository. Do not use Codex Lab to work on Codex Lab, and do not invent a
task only for the canary. Record pass/fail and bounded metadata for each item;
do not include prompt, response, token, account, business context, or private
repository contents.

- [ ] Install `codex-lab-v0.1.0-lab.4` with the no-checkout command and confirm
      installer status plus structured provenance.
- [ ] Authenticate, confirm existing accounts are preserved, add or select a
      second account if available, and verify intentional account switching. If
      #519 applies, keep the healthy account active, use manual selection, and
      record whether `/settings` or the session-only `-c` override was used.
      Treat automatic switching as a known limitation rather than repeatedly
      refreshing a terminally invalid account.
- [ ] Launch with understood permissions and record the selected sandbox and
      approval policy.
- [ ] Complete one meaningful code-changing turn for the selected real task,
      then inspect the resulting diff without copying proprietary content into
      the canary report.
- [ ] Complete one bounded operation with an installed third-party agent and
      record its exact selector plus a non-empty terminal result.
- [ ] Observe an `Automatic Validation` cell in the turn transcript with one
      terminal state: passed, failed, configuration error, timed out,
      infrastructure failure, cancelled, or skipped. Record the state and its
      displayed reason; only `passed` is a green canary result.
- [ ] Observe the `Background Review` cell in the turn transcript and wait for
      completed, failed, cancelled, superseded, or skipped. Record the reasoned
      summary or finding count. A successful headline may report completion, no
      findings, one finding, or multiple findings. Confirm the detail is terminal
      `completed` rather than relying on the headline alone. Any completed run
      with visible detail is a green canary result, including zero findings.
      Background Review may finish after the turn's response; if nothing is
      visible yet, report only that no run was observable at that point and keep
      waiting rather than calling it skipped.
- [ ] Exit, restart the supervisor, and resume the same thread.
- [ ] Roll back to `codex-lab-v0.1.0-lab.3`, verify exact provenance and health,
      and optionally reinstall `codex-lab-v0.1.0-lab.4`.

## Leave the private dogfood program

After completing the canary and any requested rollback evidence, remove the
recorded Codex Lab installation through the same reviewed installer:

```sh
installer_commit=902f6ddd7a797ce432c6e635552b854dd53ce00a
run_codex_lab_installer "$installer_commit" --uninstall
```

Uninstall removes the recorded app, shim, managed engine, installer state, and
Codex Lab supervisor, and restores a prior managed engine when one was recorded.
It intentionally preserves `~/.codex-lab`, including authentication and session
state. Do not delete that directory without separately reviewing what private
state must be retained or securely removed.

## Report a failure safely

Report the release tag, source commit from structured provenance, macOS version,
architecture, failing checklist step, exact command name, exit status, and a
short redacted diagnostic. Say whether install, launch, the core harness turn,
or rollback is blocked. Do not attach full logs until they have been reviewed
for credentials, account identity, prompts, responses, private paths, URLs, and
repository content.
