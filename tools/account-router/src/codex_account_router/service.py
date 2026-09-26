"""Foreground ownership of an isolated stock phone host and its account router."""

import asyncio
import logging
import os
import plistlib
import signal
import sys
from contextlib import AsyncExitStack, ExitStack
from dataclasses import dataclass
from logging.handlers import RotatingFileHandler
from pathlib import Path
from typing import cast

import aiohttp

from .accounts import AccountError, Lease, private_directory, read_metadata, worker_environment
from .rpc import ConnectionLost, Rpc, RpcError
from .server import serve


@dataclass(frozen=True)
class Service:
    codex: str
    data_dir: Path
    control_home: Path
    control_account_id: str
    accounts: list[str]
    port: int = 41979

    @property
    def control_socket(self):
        return self.control_home / "host.sock"

    @classmethod
    def load(cls, path):
        try:
            values = read_metadata(path)
            for key in ("data_dir", "control_home", "codex"):
                value = Path(values[key])
                if not value.is_absolute() or value.resolve() != value:
                    raise ValueError("service paths must be absolute and resolved")
                values[key] = str(value) if key == "codex" else value
            service = cls(**values)
            if (
                service.control_home.parent != service.data_dir
                or service.control_home.name == "accounts"
                or service.control_home == Path.home() / ".codex"
                or not isinstance(service.control_account_id, str)
                or not service.control_account_id
                or not isinstance(service.accounts, list)
                or not service.accounts
                or not all(isinstance(label, str) for label in service.accounts)
                or type(service.port) is not int
                or not 0 < service.port < 65536
            ):
                raise ValueError("invalid isolated service settings")
            return service
        except (KeyError, TypeError, ValueError):
            raise AccountError("invalid service configuration; see the package README") from None


def launch_agent(config: Path, service: Service):
    """Render a user agent without installing it or touching existing services."""
    return plistlib.dumps(
        {
            "Label": "com.codex.account-router",
            "ProgramArguments": [
                sys.executable,
                "-m",
                "codex_account_router",
                "service",
                str(config.resolve()),
            ],
            "WorkingDirectory": str(service.data_dir),
            "EnvironmentVariables": {"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
            "RunAtLoad": True,
            "KeepAlive": True,
            "ThrottleInterval": 30,
            "ExitTimeOut": 60,
            "Umask": 0o077,
            "StandardOutPath": "/dev/null",
            "StandardErrorPath": "/dev/null",
        },
        sort_keys=False,
    ).decode()


async def refresh_subscriptions(rpc, data_dir, subscribed):
    entries = read_metadata(data_dir / "selection.json")
    desired = set(
        [thread for thread, entry in entries.items() if "inheritedFrom" not in entry][-100:]
    )
    for thread in desired - subscribed:
        resumed = await rpc.call(
            "thread/resume",
            {"threadId": thread, "modelProvider": "account-router", "excludeTurns": True},
        )
        if resumed.get("modelProvider") != "account-router":
            raise AccountError(
                "registered task has a different active provider; operator review needed"
            )
        subscribed.add(thread)
    for thread in subscribed - desired:
        await rpc.call("thread/unsubscribe", {"threadId": thread})
        subscribed.remove(thread)


async def start_router(service, stopped):
    async with AsyncExitStack() as stack:
        async with asyncio.timeout(30):
            while True:
                try:
                    rpc = await Rpc.local(service.control_socket)
                    stack.push_async_callback(rpc.close)
                    break
                except (OSError, aiohttp.ClientError, ConnectionLost):
                    await asyncio.sleep(0.1)
            account = await rpc.call("account/read", {"refreshToken": False})
            identity = (account.get("workspaceRouting") or {}).get("chatgptAccountId")
            if identity != service.control_account_id:
                raise AccountError("control identity changed; review the service configuration")
        subscribed = set()
        await refresh_subscriptions(rpc, service.data_dir, subscribed)
        await rpc.call("remoteControl/enable", {"ephemeral": True})
        router = asyncio.create_task(serve(service, stopped=stopped))
        try:
            while not router.done():
                await asyncio.wait([router], timeout=5)
                if not router.done():
                    account = await rpc.call("account/read", {"refreshToken": False})
                    identity = (account.get("workspaceRouting") or {}).get("chatgptAccountId")
                    if identity != service.control_account_id:
                        raise AccountError(
                            "control identity changed; review the service configuration"
                        )
                    await refresh_subscriptions(rpc, service.data_dir, subscribed)
            await router
        finally:
            router.cancel()
            await asyncio.gather(router, return_exceptions=True)


async def supervise(service, stopped, logger):
    private_directory(service.data_dir)
    private_directory(service.control_home)
    with ExitStack() as stack:
        owner = Lease(service.control_home / "owner.lock")
        stack.callback(owner.close)
        if not (service.control_home / "auth.json").is_file():
            raise AccountError("complete the isolated control login before starting the service")
        # An existing listener is never adopted or replaced, even if a prior
        # foreground operator did not participate in our ownership lock.
        try:
            _, writer = await asyncio.open_unix_connection(service.control_socket)
        except (FileNotFoundError, ConnectionRefusedError):
            pass
        else:
            writer.close()
            await writer.wait_closed()
            raise AccountError("control socket is already serving; stop its owner first")
        process = await asyncio.create_subprocess_exec(
            service.codex,
            "app-server",
            "--listen",
            "unix://" + str(service.control_socket),
            "-c",
            'cli_auth_credentials_store="file"',
            cwd=service.control_home,
            env=worker_environment(service.control_home),
            pass_fds=(owner.fd,),
            stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT,
        )
        logger.info("Started isolated stock host pid=%s", process.pid)
        output_stream = cast(asyncio.StreamReader, process.stdout)

        async def capture():
            while chunk := await output_stream.read(16384):
                logger.info("stock: %s", chunk.decode(errors="replace").rstrip())

        output = asyncio.create_task(capture())
        host = asyncio.create_task(process.wait())
        router = asyncio.create_task(start_router(service, stopped))
        stop = asyncio.create_task(stopped.wait())
        try:
            done, _ = await asyncio.wait([host, router, stop], return_when=asyncio.FIRST_COMPLETED)
            if stop not in done:
                if router in done:
                    await router
                raise AccountError("a service component exited; supervisor will restart")
        finally:
            stopped.set()
            router.cancel()
            await asyncio.gather(router, return_exceptions=True)
            if process.returncode is None:
                process.terminate()
                try:
                    await asyncio.wait_for(asyncio.shield(host), 20)
                except TimeoutError:
                    process.kill()
            await host
            try:
                await asyncio.wait_for(output, 5)
            except TimeoutError:
                logger.warning("Stock output pipe did not close after shutdown")
            stop.cancel()
            await asyncio.gather(stop, return_exceptions=True)
            logger.info("Stopped isolated stock host")


async def run_service(service):
    os.umask(0o077)
    private_directory(service.data_dir)
    log_dir = service.data_dir / "logs"
    private_directory(log_dir)
    handler = RotatingFileHandler(log_dir / "service.log", maxBytes=1024 * 1024, backupCount=3)
    handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
    logger = logging.getLogger("account-router-service")
    logger.setLevel(logging.INFO)
    logger.addHandler(handler)
    stopped = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stopped.set)
    try:
        await supervise(service, stopped, logger)
    except (AccountError, RpcError, OSError, TimeoutError) as error:
        logger.error("Service failed: %s", error)
        raise
    finally:
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.remove_signal_handler(sig)
        logger.removeHandler(handler)
        handler.close()
