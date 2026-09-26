"""One router process with a private Unix administration socket."""

import asyncio
import os
import signal
from contextlib import AsyncExitStack
from functools import partial

import aiohttp
from aiohttp import web

from .accounts import AccountError, AccountWorker, Lease, private_directory
from .control import Control
from .proxy import Proxy, application
from .rpc import RpcError
from .selection import Selection


def admin_application(selection):
    async def handle(request):
        try:
            if request.path == "/status" and request.method == "GET":
                return web.json_response(await selection.status())
            if request.path == "/select" and request.method == "POST":
                data = await request.json()
                return web.json_response(await selection.select(data["thread"], data["execution"]))
            raise web.HTTPNotFound()
        except AccountError as error:
            return web.json_response({"error": str(error)}, status=409)
        except (KeyError, TypeError, ValueError):
            return web.json_response({"error": "invalid selection request"}, status=400)
        except (RpcError, TimeoutError):
            return web.json_response({"error": "stock server unavailable"}, status=503)

    app = web.Application(client_max_size=65536)
    app.router.add_route("*", "/{path:.*}", handle)
    return app


async def serve(args):
    private_directory(args.data_dir)
    async with AsyncExitStack() as stack:
        lease = Lease(args.data_dir / "router.lock")
        stack.callback(lease.close)
        control = Control(args.control_socket)
        stack.push_async_callback(control.close)
        await control.connection()
        workers = {}
        identities = {control.owner_id}
        for label in dict.fromkeys(args.accounts):
            worker = await AccountWorker.start(
                args.data_dir, label, args.codex, excluded_ids=identities
            )
            stack.push_async_callback(worker.close)
            identities.add((await worker.credentials()).account_id)
            workers[label] = worker
        selection = Selection(args.data_dir, control, workers)
        session = await stack.enter_async_context(
            aiohttp.ClientSession(
                auto_decompress=False,
                timeout=aiohttp.ClientTimeout(total=None, sock_connect=30, sock_read=600),
            )
        )
        proxy = web.AppRunner(
            application(Proxy(control, selection, workers, session)),
            auto_decompress=False,
            access_log=None,
            handler_cancellation=True,
        )
        await proxy.setup()
        stack.push_async_callback(proxy.cleanup)
        await web.TCPSite(proxy, "127.0.0.1", args.port).start()
        socket_path = args.data_dir / "control.sock"
        if socket_path.exists():
            if not socket_path.is_socket() or socket_path.stat().st_uid != os.getuid():
                raise AccountError("refusing to replace an unrelated control socket")
            socket_path.unlink()
        admin = web.AppRunner(admin_application(selection), access_log=None)
        await admin.setup()
        stack.push_async_callback(admin.cleanup)
        # Private parent directory protects the socket even before chmod.
        await web.UnixSite(admin, str(socket_path)).start()
        socket_path.chmod(0o600)
        stack.callback(partial(socket_path.unlink, missing_ok=True))
        stopped = asyncio.Event()
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, stopped.set)
            stack.callback(loop.remove_signal_handler, sig)
        print(
            f"Account router ready on loopback:{args.port}; execution labels: {', '.join(workers)}",
            flush=True,
        )
        await stopped.wait()
