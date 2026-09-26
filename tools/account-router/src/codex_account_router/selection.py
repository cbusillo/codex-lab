"""Durable per-task choices, checked against the task-owning stock server."""

import asyncio
from contextlib import asynccontextmanager
from pathlib import Path

from .accounts import AccountError, read_metadata, write_metadata
from .rpc import RpcError


class Selection:
    def __init__(self, root: Path, rpc, labels):
        self.path = root / "selection.json"
        self.rpc = rpc
        self.labels = set(labels)
        self.entries = read_metadata(self.path)
        if any(
            not isinstance(entry, dict)
            or not isinstance(entry.get("label"), str)
            or not isinstance(entry.get("name"), str)
            for entry in self.entries.values()
        ):
            raise AccountError("invalid task selection metadata; preserve it for repair")
        self.locks = {}
        self.active = {}

    def save(self):
        write_metadata(self.path, self.entries)

    def lock(self, thread_id):
        return self.locks.setdefault(thread_id, asyncio.Lock())

    async def thread(self, thread_id):
        result = await self.rpc.call("thread/read", {"threadId": thread_id, "includeTurns": False})
        thread = result["thread"]
        if thread["id"] != thread_id or thread["modelProvider"] != "account-router":
            raise AccountError("task does not use the account-router provider")
        return thread

    async def select(self, thread_id, label):
        if label not in self.labels:
            raise AccountError("unknown execution label")
        async with self.lock(thread_id):
            thread = await self.thread(thread_id)
            # Stock reports systemError for an idle task after a failed turn;
            # Active takes precedence while a turn/approval is still running.
            if thread["status"]["type"] not in (
                "idle",
                "notLoaded",
                "systemError",
            ) or self.active.get(thread_id):
                raise AccountError("task is busy; select an account between turns")
            entry = self.entries.pop(thread_id, {})
            self.entries[thread_id] = entry
            entry.pop("inheritedFrom", None)
            entry.update(
                label=label, name=(thread.get("name") or thread["preview"] or thread_id)[:120]
            )
            self.save()
            return {"thread": thread_id, "execution": label}

    async def inherit(self, thread_id, depth=0):
        thread = await self.thread(thread_id)
        entry = self.entries.get(thread_id)
        if entry is not None and "inheritedFrom" not in entry:
            return thread, entry
        parent = thread.get("parentThreadId")
        if not parent or depth >= 32:
            raise AccountError("task has no execution selection")
        # Re-evaluate inheritance each turn, unless the child has an explicit
        # selection. Never trust a caller-supplied parent header.
        parent_thread, ancestor = await self.inherit(parent, depth + 1)
        label = ancestor["label"]
        if parent_thread["status"]["type"] == "active":
            label = ancestor.get("pinLabel", label)
        entry = dict(
            entry or {},
            label=label,
            inheritedFrom=parent,
            name=(thread.get("name") or thread["preview"] or thread_id)[:120],
        )
        return thread, entry

    @asynccontextmanager
    async def route(self, thread_id, turn_id):
        async with self.lock(thread_id):
            _, entry = await self.inherit(thread_id)
            self.entries[thread_id] = entry
            if entry.get("pinTurn") != turn_id:
                entry.update(pinTurn=turn_id, pinLabel=entry["label"])
                self.save()
            if entry.get("lastRequest") is not None:
                entry["lastRequest"] = None
                self.save()
            label = entry["pinLabel"]
            if label not in self.labels:
                raise AccountError("selected execution account is unavailable")
            self.active[thread_id] = self.active.get(thread_id, 0) + 1
        try:
            yield label
        finally:
            # No await: cancellation cannot strand the task's active counter.
            self.active[thread_id] -= 1
            if not self.active[thread_id]:
                del self.active[thread_id]

    async def receipt(self, thread_id, label, status):
        async with self.lock(thread_id):
            receipt = {
                "execution": label,
                "httpStatus": status,
            }
            if self.entries[thread_id].get("lastRequest") != receipt:
                self.entries[thread_id]["lastRequest"] = receipt
                self.save()

    async def status(self):
        snapshot = [
            (key, dict(value))
            for key, value in self.entries.items()
            if "inheritedFrom" not in value
        ][-100:]
        limit = asyncio.Semaphore(8)

        async def row(thread_id, entry):
            async with limit:
                try:
                    async with asyncio.timeout(3):
                        thread, entry = await self.inherit(thread_id)
                        state = thread["status"]["type"]
                except (RpcError, AccountError, TimeoutError):
                    state = "unavailable"
                return {
                    "thread": thread_id,
                    "execution": entry.get("pinLabel", entry["label"])
                    if state == "active"
                    else entry["label"],
                    "name": entry["name"],
                    "state": state,
                    "lastRequest": entry.get("lastRequest") if state != "unavailable" else None,
                }

        rows = await asyncio.gather(*(row(key, entry) for key, entry in snapshot))
        return {"accounts": sorted(self.labels), "tasks": rows}
