# Repo Skills Provenance

The repo-local skills in this directory are owned by this Codex checkout. They
include the Every Code skill set from `just-every/code` while preserving local
Codex-specific additions and fixes.

## Just Every Source

- Upstream: `https://github.com/just-every/code`
- Compared ref: `just-every/main`
- Compared commit: `e0d78a4e390e021317ec68322e87d4bfb6316e0d`
- Compared date: 2026-06-08
- Upstream license: Apache-2.0

`just-every/code` is an Every Code fork of `openai/codex`. Treat its skills as a
source to review and selectively sync, not as an external runtime dependency.

## Sync Status

The following Every Code repo skills are present locally:

- `babysit-pr`
- `code-review`
- `code-review-breaking-changes`
- `code-review-change-size`
- `code-review-context`
- `code-review-testing`
- `codex-bug`
- `codex-issue-digest`
- `codex-pr-body`
- `pushing-ci-changes`
- `remote-tests`
- `test-tui`

Local Codex-only skills:

- `update-v8-version`

Known intentional local deltas from the compared Every Code ref:

- `codex-issue-digest` uses collector v5 semantics for unique human-user
  interaction counts and includes the corresponding tests.
- `update-v8-version` is local to this Codex checkout.

## Future Sync Procedure

1. Fetch the source ref without adding a persistent remote:

   ```bash
   git fetch https://github.com/just-every/code.git main:refs/remotes/just-every/main
   ```

2. Review drift before copying anything:

   ```bash
   git diff --name-status just-every/main -- .codex/skills
   git diff just-every/main -- .codex/skills
   ```

3. Preserve local Codex-specific skills and fixes unless the replacement is
   deliberate and documented in the PR.
4. Keep imports scoped to `.codex/skills`; plugins, marketplaces, and external
   skill catalogs belong in their own tracked workstreams.
