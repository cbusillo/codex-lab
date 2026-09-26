"""Isolated stock-owned logins for Unix control-socket hosts (macOS/Linux)."""

import asyncio
import base64
import fcntl
import json
import os
import re
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlsplit

from .rpc import ConnectionLost, Rpc


class AccountError(RuntimeError):
    pass


def read_metadata(path: Path):
    if not path.exists():
        return {}
    try:
        if path.stat().st_size > 4 * 1024 * 1024:
            raise ValueError("oversized metadata")
        value = json.loads(path.read_text())
        if not isinstance(value, dict):
            raise ValueError("invalid metadata")
        return value
    except (ValueError, TypeError):
        raise AccountError(
            f"invalid router metadata in {path.name}; preserve it for repair"
        ) from None


def write_metadata(path: Path, value):
    temporary = path.with_suffix(".tmp")
    with os.fdopen(
        os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600), "w"
    ) as stream:
        json.dump(value, stream)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


class Lease:
    """Serialize router, worker, and enrollment ownership with kernel locks."""

    def __init__(self, path: Path):
        self.fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            os.close(self.fd)
            raise AccountError("account or router is already in use") from None

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


def private_directory(path: Path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    if path.is_symlink() or path.stat().st_uid != os.getuid():
        raise AccountError("data directory must be owned by this user and not a symlink")
    path.chmod(0o700)


def account_home(root: Path, label: str):
    if not re.fullmatch(r"[a-z][a-z0-9-]{0,31}", label):
        raise AccountError("account label must be 1-32 lowercase letters, digits, or hyphens")
    private_directory(root)
    parent = root / "accounts"
    private_directory(parent)
    home = parent / label
    private_directory(home)
    return home


def worker_environment(home: Path):
    # Do not inherit API keys, access tokens, federation settings, or another
    # Codex installation's auth overrides into a managed execution login.
    env = {
        key: os.environ[key]
        for key in (
            "PATH",
            "HOME",
            "USER",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "CODEX_CA_CERTIFICATE",
            "SSL_CERT_FILE",
        )
        if key in os.environ
    }
    env["CODEX_HOME"] = str(home)
    return env


@dataclass(frozen=True)
class Credentials:
    token: str = field(repr=False)
    account_id: str = field(repr=False)
    origin: str
    routing: str


def credentials_from(exported, account):
    token = exported.get("authToken")
    route = account.get("workspaceRouting") or {}
    account_id = route.get("chatgptAccountId")
    if (
        exported.get("authMethod") != "chatgpt"
        or not isinstance(token, str)
        or not token
        or not isinstance(account_id, str)
        or not account_id
    ):
        raise AccountError("execution account needs a stock ChatGPT login")
    try:
        encoded = token.split(".")[1]
        claims = json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)))
        if claims["https://api.openai.com/auth"]["chatgpt_account_id"] != account_id:
            raise ValueError("account mismatch")
        origin = route["backendOrigin"]
        parsed = urlsplit(origin)
        if (
            parsed.scheme != "https"
            or parsed.hostname
            not in {
                "chatgpt.com",
                "us.chatgpt.com",
                "gov.chatgpt.com",
            }
            or parsed.port not in (None, 443)
            or parsed.username
            or parsed.password
            or parsed.path not in ("", "/")
            or parsed.query
            or parsed.fragment
        ):
            raise ValueError("unsupported backend")
        routing = route["accountRoutingOverride"]
        if routing not in ("NO_CONSTRAINT", "us", "us_cr"):
            raise ValueError("unsupported routing")
    except (ValueError, KeyError, IndexError, TypeError):
        raise AccountError("execution credential or backend identity is invalid") from None
    return Credentials(token, account_id, origin.rstrip("/"), routing)


class AccountWorker:
    def __init__(self, home, lease, rpc, *, codex=None, excluded_ids=()):
        self.home = home
        self.lease = lease
        self.rpc = rpc
        self.codex = codex
        self.excluded_ids = frozenset(excluded_ids)
        self._refresh = asyncio.Lock()
        self._last_rejected_token = None
        identity = home / "router-identity.json"
        self._account_id = read_metadata(identity).get("account_id")
        if identity.exists() and (not isinstance(self._account_id, str) or not self._account_id):
            raise AccountError("invalid execution identity metadata; preserve it for repair")

    @staticmethod
    async def connect(codex, home, lease):
        return await Rpc.worker(
            [codex, "app-server", "--stdio", "-c", 'cli_auth_credentials_store="file"'],
            home=home,
            env=worker_environment(home),
            pass_fds=(lease.fd,),
        )

    @classmethod
    async def start(cls, root: Path, label: str, codex: str, *, excluded_ids=()):
        home = account_home(root, label)
        lease = Lease(home / "worker.lock")
        try:
            if not (home / "auth.json").is_file():
                raise AccountError(f"execution account {label} is not enrolled")
            rpc = await cls.connect(codex, home, lease)
            try:
                return cls(home, lease, rpc, codex=codex, excluded_ids=excluded_ids)
            except BaseException:
                await rpc.close()
                raise
        except BaseException:
            lease.close()
            raise

    async def credentials(self, *, refresh=False, rejected_token=None):
        async with self._refresh:
            for attempt in range(2):
                try:
                    return await self.export(refresh=refresh, rejected_token=rejected_token)
                except ConnectionLost:
                    if attempt or self.codex is None:
                        raise
                    await self.rpc.close()
                    self.rpc = await self.connect(self.codex, self.home, self.lease)
        raise AssertionError("bounded worker read did not return")

    async def export(self, *, refresh, rejected_token):
        exported = await self.rpc.call(
            "getAuthStatus", {"includeToken": True, "refreshToken": False}
        )
        # Concurrent 401s from one expired token share the worker's first refresh.
        if (
            refresh
            and exported.get("authToken") == rejected_token
            and rejected_token != self._last_rejected_token
        ):
            self._last_rejected_token = rejected_token
            exported = await self.rpc.call(
                "getAuthStatus", {"includeToken": True, "refreshToken": True}
            )
        account = await self.rpc.call("account/read", {"refreshToken": False})
        credential = credentials_from(exported, account)
        if credential.account_id in self.excluded_ids:
            raise AccountError(
                "execution login must differ from control and other execution accounts"
            )
        if self._account_id and credential.account_id != self._account_id:
            raise AccountError("execution login changed identity; enrollment review required")
        if self._account_id is None:
            path = self.home / "router-identity.json"
            write_metadata(path, {"account_id": credential.account_id})
            self._account_id = credential.account_id
        return credential

    async def close(self):
        try:
            await self.rpc.close()
        finally:
            self.lease.close()


async def enroll(root: Path, label: str, codex: str):
    home = account_home(root, label)
    lease = Lease(home / "worker.lock")
    process = None
    try:
        # Login UI and refresh-token persistence are entirely stock Codex's work.
        process = await asyncio.create_subprocess_exec(
            codex,
            "login",
            "--device-auth",
            "-c",
            'cli_auth_credentials_store="file"',
            cwd=home,
            env=worker_environment(home),
            pass_fds=(lease.fd,),
        )
        if await process.wait():
            raise AccountError(f"stock login for {label} did not complete")
    finally:
        if process is not None and process.returncode is None:
            process.terminate()
            await process.wait()
        lease.close()
