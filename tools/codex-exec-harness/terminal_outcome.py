"""Typed terminal outcome helpers for exec-harness scenarios."""

from dataclasses import dataclass


TERMINAL_OUTCOMES = (
    "passed",
    "policy_failed",
    "model_failed",
    "runner_failed",
)

TERMINAL_REASONS = (
    "assertion_mismatch",
    "missing_final_message",
    "malformed_output",
    "tool_loop",
    "provider_timeout",
    "provider_unavailable",
    "malformed_jsonl",
    "binary_verification_failed",
    "harness_error",
)


@dataclass(frozen=True)
class TerminalOutcome:
    """Bounded classification for a harness scenario run."""

    outcome: str
    reason: str
    detail: str | None = None


def passed() -> TerminalOutcome:
    return TerminalOutcome("passed", "passed")


def policy_failed(detail: str) -> TerminalOutcome:
    return TerminalOutcome("policy_failed", "assertion_mismatch", detail)


def model_failed(reason: str, detail: str | None = None) -> TerminalOutcome:
    if reason not in {"missing_final_message", "malformed_output", "tool_loop"}:
        raise ValueError(f"unsupported model failure reason: {reason}")
    return TerminalOutcome("model_failed", reason, detail)


def runner_failed(reason: str, detail: str | None = None) -> TerminalOutcome:
    if reason not in {
        "provider_timeout",
        "provider_unavailable",
        "malformed_jsonl",
        "binary_verification_failed",
        "harness_error",
    }:
        raise ValueError(f"unsupported runner failure reason: {reason}")
    return TerminalOutcome("runner_failed", reason, detail)
