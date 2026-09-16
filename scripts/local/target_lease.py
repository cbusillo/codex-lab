"""Validate and forward an inherited target-ownership lease on POSIX."""

import os
import stat
from collections.abc import Mapping


TARGET_LEASE_FD_ENV = "CODEX_LAB_TARGET_LEASE_FD"


def subprocess_lease_kwargs(
    environment: Mapping[str, str] | None = None,
) -> dict[str, tuple[int, ...]]:
    """Return subprocess keyword arguments needed to preserve a lease.

    An absent lease keeps existing subprocess behavior. A present lease must
    be a live, inheritable regular-file descriptor; malformed lease
    configuration fails closed instead of being silently dropped.
    """

    if os.name == "nt":
        return {}
    source = os.environ if environment is None else environment
    raw_fd = source.get(TARGET_LEASE_FD_ENV)
    if raw_fd is None:
        if environment is not None and TARGET_LEASE_FD_ENV in os.environ:
            raise ValueError(
                f"{TARGET_LEASE_FD_ENV} was stripped from the child environment"
            )
        return {}
    if (
        not raw_fd.isascii()
        or not raw_fd.isdecimal()
        or len(raw_fd) > 10
        or str(int(raw_fd)) != raw_fd
    ):
        raise ValueError(f"{TARGET_LEASE_FD_ENV} must be a canonical file descriptor")
    fd = int(raw_fd)
    if fd < 3 or fd > (1 << 31) - 1:
        raise ValueError(f"{TARGET_LEASE_FD_ENV} is outside the supported range")
    try:
        info = os.fstat(fd)
    except OSError as error:
        raise ValueError(f"{TARGET_LEASE_FD_ENV} is not an open descriptor") from error
    if not stat.S_ISREG(info.st_mode):
        raise ValueError(f"{TARGET_LEASE_FD_ENV} does not reference a regular file")
    if not os.get_inheritable(fd):
        raise ValueError(f"{TARGET_LEASE_FD_ENV} is not inheritable")
    return {"pass_fds": (fd,)}
