"""Sample shared-host free space without changing measured command execution."""

import threading
from pathlib import Path

from build_storage import storage_snapshot


class StorageSampler:
    """Keep bounded aggregates, not a time series or command-attributed peak."""

    def __init__(self, paths: dict[str, Path], interval_seconds: float = 1.0):
        self.paths = paths
        self.interval = interval_seconds
        self.stopped = threading.Event()
        self.initial_sampled = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.result = {
            "status": "not-requested" if not paths else "available",
            "attribution": "shared-host",
            "sampleIntervalMs": round(interval_seconds * 1000),
            "sampleCount": 0,
            "filesystems": {},
            "paths": {},
        }

    def sample(self):
        try:
            snapshot = storage_snapshot(self.paths)
            self.result["sampleCount"] += 1
            if self.result["paths"] and self.result["paths"] != snapshot["paths"]:
                self.result["status"] = "degraded"
            self.result["paths"] = snapshot["paths"]
            for item in snapshot["paths"].values():
                if item["status"] != "available":
                    self.result["status"] = "degraded"
            for key, item in snapshot["filesystems"].items():
                if item["status"] != "available":
                    self.result["status"] = "degraded"
                    continue
                if key not in self.result["filesystems"]:
                    # A changing mount must not grow evidence without bound.
                    if len(self.result["filesystems"]) >= len(self.paths):
                        self.result["status"] = "degraded"
                        continue
                    self.result["filesystems"][key] = {
                        "totalBytes": item["totalBytes"],
                        "observedFreeBytesFirst": item["observedFreeBytes"],
                        "observedFreeBytesMin": item["observedFreeBytes"],
                        "sampleCount": 0,
                    }
                aggregate = self.result["filesystems"][key]
                if aggregate["totalBytes"] != item["totalBytes"]:
                    self.result["status"] = "degraded"
                aggregate["observedFreeBytesLast"] = item["observedFreeBytes"]
                aggregate["observedFreeBytesMin"] = min(
                    aggregate["observedFreeBytesMin"], item["observedFreeBytes"]
                )
                aggregate["sampleCount"] += 1
        except Exception:
            # Optional telemetry must not prevent a build or change its exit.
            self.result["status"] = "degraded"

    def _run(self):
        self.sample()
        self.initial_sampled.set()
        while not self.stopped.wait(self.interval):
            self.sample()
        self.sample()

    def start(self):
        if self.paths:
            try:
                self.thread.start()
            except RuntimeError:
                self.result["status"] = "degraded"
                self.result["reason"] = "sampler-start-failed"
                return
            if not self.initial_sampled.wait(timeout=0.1):
                self.result["status"] = "degraded"

    def finish(self):
        if self.paths and self.thread.ident is not None:
            self.stopped.set()
            self.thread.join(timeout=2)
            if self.thread.is_alive():
                # A stalled filesystem lookup must not delay a finished build.
                return {
                    "status": "degraded",
                    "attribution": "shared-host",
                    "reason": "sampler-timeout",
                    "sampleIntervalMs": round(self.interval * 1000),
                    "sampleCount": 0,
                    "paths": {},
                    "filesystems": {},
                }
        return self.result
