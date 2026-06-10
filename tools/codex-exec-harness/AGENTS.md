# Codex Exec Harness Agent Guidance

Use this harness for black-box `codex exec` proof when a change affects prompt
assembly, skills, project instructions, config loading, or request shape.

Keep scenarios narrow and deterministic. Prefer fake `/v1/responses` assertions
when the behavior can be proven from the outbound request body. Use live or
local model runs separately when model behavior is the actual question.

For context-bloat regressions, prefer exact-count assertions over broad
contains/not-contains checks. A project doc, skill listing, or large injected
fragment should appear the intended number of times, usually once.

Do not grow this into the Every Code Lab harness by default. Add fake services,
multi-turn replay, auth inheritance, or local-model helpers only when a Codex
change needs that proof and the scenario cannot be expressed more simply.
