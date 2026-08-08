"""Stable release contract for the managed Codex Lab engine."""

ENGINE_ARCHIVE_ROOT = "engine"
ENGINE_CLI_ARCHIVE_PATH = f"{ENGINE_ARCHIVE_ROOT}/codex"
CODE_MODE_HOST_ARCHIVE_PATH = f"{ENGINE_ARCHIVE_ROOT}/codex-code-mode-host"
ENGINE_SIGNING_IDENTIFIER = "com.shinycomputers.codex-lab.engine"
CODE_MODE_HOST_SIGNING_IDENTIFIER = "com.shinycomputers.codex-lab.code-mode-host"
ENGINE_TEAM_IDENTIFIER = "MM5YXC7T6E"
REQUIRED_ENGINE_ENTITLEMENTS = ("com.apple.security.cs.allow-jit",)
REQUIRED_CODE_MODE_HOST_ENTITLEMENTS = (
    "com.apple.security.cs.allow-jit",
    "com.apple.security.cs.allow-unsigned-executable-memory",
)
