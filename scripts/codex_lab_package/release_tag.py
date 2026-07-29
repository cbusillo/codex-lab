"""Codex Lab release-tag parsing and ordering."""

import re


LAB_RELEASE_TAG_PREFIX = "codex-lab-v"
LAB_RELEASE_TAG_PATTERN = re.compile(
    r"^codex-lab-v(?P<major>[0-9]+)\.(?P<minor>[0-9]+)\.(?P<patch>[0-9]+)"
    r"(?:-lab\.(?P<lab>[0-9]+))?$"
)


def release_version_from_tag(tag: str) -> str:
    match = LAB_RELEASE_TAG_PATTERN.fullmatch(tag)
    if match is None:
        raise ValueError(f"Codex Lab release tag has invalid format: {tag}")
    return ".".join((match.group("major"), match.group("minor"), match.group("patch")))


def codex_lab_release_order(tag: str) -> tuple[int, int, int, int, int] | None:
    match = LAB_RELEASE_TAG_PATTERN.fullmatch(tag)
    if match is None:
        return None
    major = int(match.group("major"))
    minor = int(match.group("minor"))
    patch = int(match.group("patch"))
    lab = match.group("lab")
    if lab is None:
        return (major, minor, patch, 1, 0)
    return (major, minor, patch, 0, int(lab))
