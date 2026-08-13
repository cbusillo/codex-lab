"""Repeated-tool-call loop detection for exec-harness scenarios."""


def detect_repeated_command_loop(
    commands: list[str], consecutive_threshold: int
) -> str | None:
    if consecutive_threshold < 2:
        raise ValueError("consecutive_threshold must be at least 2")

    previous = None
    streak = 0
    for command in commands:
        if command == previous:
            streak += 1
        else:
            previous = command
            streak = 1
        if streak >= consecutive_threshold:
            return command
    return None
