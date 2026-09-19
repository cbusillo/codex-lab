# Review hook (spike)

Spike for [#950](https://github.com/cbusillo/codex-lab/issues/950), part of the thin-fork
decision in [#926](https://github.com/cbusillo/codex-lab/issues/926). Background review as a
Codex Stop hook, with no engine changes and no third-party plugin.

When a turn ends with uncommitted changes that have not been reviewed yet, `review_hook.py`
runs stock Codex's own reviewer, `codex review --uncommitted`, in the workspace. If it reports
findings (`- [P1] …`), the hook answers `{"decision": "block", "reason": …}` and the agent
continues and addresses them. Standard library only.

```toml
# ~/.codex/config.toml
[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "python3 /abs/path/review_hook.py"
timeout = 600
statusMessage = "Background review"
```

Codex asks once to trust a new hook. Non-interactive runs need
`--dangerously-bypass-hook-trust` until it has been trusted in the TUI.

Behaviour worth knowing:

- The turn waits for the review. One review of a small change took 54 s.
- A change set is reviewed once: a fingerprint of `git status`, `git diff HEAD` and untracked
  file contents is recorded under `~/.cache/lab-review-hook`.
- It reviews only once per stop chain (`stop_hook_active`), so a finding the agent cannot fix
  does not loop.
- The reviewer is a Codex run too; `LAB_REVIEW_HOOK_ACTIVE` stops it from triggering itself.
- The reviewer runs with the workspace as its directory, so stock Codex applies the project's
  `AGENTS.md` to it the normal way.
