import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/upstream_convergence_repair_ledger.py"
LOCAL = "1" * 40
BASE = "2" * 40
UPSTREAM = "3" * 40


class RepairLedgerTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.packets = self.root / "model-packets.json"
        self.telemetry = self.root / "model-telemetry.json"
        self.evidence = self.root / "candidate-evidence.json"
        self.ledger = self.root / "upstream-convergence-repair-ledger.json"
        self.output = self.root / "out"
        self.write_inputs(7)

    def tearDown(self):
        self.temp.cleanup()

    def write_inputs(self, count):
        packets = {
            "schemaVersion": 1,
            "stage": "3c",
            "packets": [{"packetId": f"packet:{index}"} for index in range(count)],
            "deferredPackets": [],
            "warnings": [],
        }
        self.packets.write_text(json.dumps(packets), encoding="utf-8")
        self.telemetry.write_text(json.dumps({
            "schemaVersion": 1,
            "stage": "3c",
            "modelFree": True,
            "invocation": "not-invoked",
            "calls": 0,
            "promptTokens": 0,
            "completionTokens": 0,
            "totalTokens": 0,
            "actualTotalTokens": 0,
        }), encoding="utf-8")
        self.evidence.write_text(json.dumps({
            "schemaVersion": 1,
            "classification": "conflict",
            "refs": {"base": BASE, "upstream": UPSTREAM, "local": LOCAL},
            "workflow": {"sha": LOCAL, "runId": "run-1"},
        }), encoding="utf-8")

    def cycle(self, unit, head, paths, calls=1, prompt=1, completion=1, status="repaired"):
        return {
            "repairUnitId": unit,
            "status": status,
            "repairHead": head,
            "touchedPaths": paths,
            "resolved": status == "repaired",
            "repairAgent": "model",
            "modelTier": "budget",
            "accountingConfidence": "provider_reported",
            "startedAt": "2026-08-28T00:00:00Z",
            "finishedAt": "2026-08-28T00:00:01Z",
            "durationMs": 1000,
            "accounting": {
                "calls": calls,
                "promptTokens": prompt,
                "completionTokens": completion,
                "totalTokens": prompt + completion,
            },
        }

    def execute(self, ledger=True, require_live=False):
        env = os.environ | {"RUNNER_TEMP": str(self.root)}
        command = [sys.executable, str(SCRIPT), "checkpoint", "--packets", str(self.packets), "--telemetry", str(self.telemetry), "--evidence", str(self.evidence), "--output-dir", str(self.output), "--cycle-id", "cycle-1"]
        if ledger:
            command += ["--ledger", str(self.ledger)]
        if require_live:
            command.append("--require-live")
        return subprocess.run(command, env=env, capture_output=True, text=True)

    def write_ledger(self, cycles, provenance="synthetic-fixture", accounting=None, **extra):
        value = {"schemaVersion": 1, "stage": "3d", "provenance": provenance, "refs": {"base": BASE, "upstream": UPSTREAM, "local": LOCAL}, "cycles": cycles, "repairHead": cycles[-1]["repairHead"] if cycles else None}
        if accounting is not None:
            value["accounting"] = accounting
        value.update(extra)
        self.ledger.write_text(json.dumps(value), encoding="utf-8")

    def checkpoint(self):
        return json.loads((self.output / "repair-checkpoint.json").read_text(encoding="utf-8"))

    def test_absent_ledger_is_zero_cycle_exact_local_sha(self):
        result = self.execute(ledger=False, require_live=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        checkpoint = self.checkpoint()
        self.assertEqual(checkpoint["decision"], "no-cycles")
        self.assertEqual(checkpoint["checkpointSha"], LOCAL)
        self.assertFalse(checkpoint["repairsPerformed"])
        self.assertEqual(checkpoint["unresolvedUnitTotal"], 7)
        self.assertEqual(checkpoint["accounting"]["totalTokens"], 0)
        evidence = json.loads(self.evidence.read_text(encoding="utf-8"))
        self.assertEqual(evidence["classification"], "conflict")

    def test_two_unrelated_components_stop(self):
        self.write_ledger([self.cycle("packet:0", "a" * 40, ["one"]), self.cycle("packet:1", "b" * 40, ["two"])])
        result = self.execute()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.checkpoint()["decision"], "handoff")
        self.assertEqual(self.checkpoint()["handoffReason"], "unrelated_cycle_cap")
        self.assertEqual(self.checkpoint()["unrelatedComponentTotal"], 2)
        self.assertEqual(self.checkpoint()["checkpointSha"], "b" * 40)

    def test_shared_touched_path_is_one_component(self):
        self.write_ledger([self.cycle("packet:0", "a" * 40, ["same"]), self.cycle("packet:1", "b" * 40, ["same", "other"])])
        result = self.execute()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.checkpoint()["unrelatedComponentTotal"], 1)
        self.assertEqual(self.checkpoint()["decision"], "continue")

    def test_attempt_cap_and_precedence(self):
        cycles = [self.cycle("packet:0", letter * 40, ["same"]) for letter in "abc"]
        self.write_ledger(cycles)
        self.assertEqual(self.execute().returncode, 0)
        self.assertEqual(self.checkpoint()["handoffReason"], "attempt_cap")
        self.assertEqual(self.checkpoint()["checkpointSha"], "c" * 40)
        cycles.append(self.cycle("packet:1", "d" * 40, ["different"], prompt=20_001, completion=20_001))
        self.write_ledger(cycles)
        self.assertEqual(self.execute().returncode, 0)
        self.assertEqual(self.checkpoint()["handoffReason"], "unrelated_cycle_cap")

    def test_unknown_refs_heads_and_accounting_fail(self):
        self.write_ledger([self.cycle("packet:unknown", "a" * 40, ["one"])])
        self.assertNotEqual(self.execute().returncode, 0)
        value = json.loads(self.ledger.read_text(encoding="utf-8"))
        value["cycles"] = [self.cycle("packet:0", "a" * 40, ["one"])]
        value["refs"]["local"] = "4" * 40
        self.ledger.write_text(json.dumps(value), encoding="utf-8")
        self.assertNotEqual(self.execute().returncode, 0)
        value["refs"]["local"] = LOCAL
        value["cycles"][0].pop("repairHead")
        self.ledger.write_text(json.dumps(value), encoding="utf-8")
        self.assertNotEqual(self.execute().returncode, 0)
        self.write_ledger([self.cycle("packet:0", LOCAL, ["one"])])
        self.assertNotEqual(self.execute().returncode, 0)
        self.write_ledger([self.cycle("packet:0", BASE, ["one"])])
        self.assertNotEqual(self.execute().returncode, 0)
        cycle = self.cycle("packet:0", "a" * 40, ["one"])
        cycle["repairAgent"], cycle["modelTier"], cycle["accountingConfidence"], cycle["accounting"] = "human", "none", "explicit_zero", {"calls": 0, "promptTokens": 0, "completionTokens": 0, "totalTokens": 0}
        self.write_ledger([cycle])
        self.assertEqual(self.execute().returncode, 0)
        self.assertEqual(self.checkpoint()["accounting"]["accountingConfidence"], "explicit_zero")
        self.write_ledger([self.cycle("packet:0", "a" * 40, ["one"], status="not-invoked")])
        self.assertNotEqual(self.execute().returncode, 0)
        self.write_ledger([self.cycle("packet:0", "a" * 40, ["one"])], accounting={"calls": 2, "promptTokens": 1, "completionTokens": 1, "totalTokens": 2})
        self.assertNotEqual(self.execute().returncode, 0)
        self.write_ledger([], repairsPerformed=True)
        self.assertNotEqual(self.execute().returncode, 0)
        self.write_ledger([self.cycle("packet:0", "a" * 40, ["one"], prompt=19_999, completion=20_000), self.cycle("packet:1", "b" * 40, ["one"], prompt=0, completion=1)])
        self.assertEqual(self.execute().returncode, 0)
        self.assertEqual(self.checkpoint()["handoffReason"], "token_budget_exhausted")
        self.assertEqual(self.checkpoint()["checkpointSha"], "b" * 40)

    def test_missing_packet_input_is_bounded_unavailable_green(self):
        self.packets.unlink()
        result = self.execute(require_live=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.checkpoint()["decision"], "no-cycles")
        self.assertEqual(self.checkpoint()["accounting"]["calls"], 0)
        self.assertEqual(self.checkpoint()["accounting"]["accountingConfidence"], "unavailable")

    def test_live_rejects_synthetic_and_oversized_ledger(self):
        self.write_ledger([])
        self.assertNotEqual(self.execute(require_live=True).returncode, 0)
        self.write_ledger([], provenance="workflow-executor")
        self.assertEqual(self.execute(require_live=True).returncode, 0)
        self.ledger.write_bytes(b"{" + b"\"x\":\"" + b"a" * (256 * 1024) + b"\"}")
        self.assertNotEqual(self.execute().returncode, 0)

    def test_bounded_stage3c_inputs_accept_valid_max_packet_volume(self):
        packets = json.loads(self.packets.read_text(encoding="utf-8"))
        packets["packets"] = [
            {"packetId": f"packet:{index}", "boundedPayload": "x" * 38_000}
            for index in range(12)
        ]
        packets["packets"][0]["packetId"] = "root:00:" + "r" * 512
        self.packets.write_text(json.dumps(packets), encoding="utf-8")
        result = self.execute(ledger=False, require_live=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_evidence_has_no_fabricated_checkpoint_sha(self):
        self.evidence.unlink()
        result = self.execute(ledger=False, require_live=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIsNone(self.checkpoint()["checkpointSha"])

    def test_attempt_cap_precedes_token_budget(self):
        cycles = [
            self.cycle("packet:0", letter * 40, ["same"], prompt=7_000, completion=7_000)
            for letter in "abc"
        ]
        self.write_ledger(cycles)
        result = self.execute()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.checkpoint()["handoffReason"], "attempt_cap")

    def test_deterministic_bounded_handoff_and_truncation(self):
        self.write_inputs(40)
        self.write_ledger([self.cycle("packet:0", "a" * 40, ["one"])])
        first = self.execute()
        handoff = (self.output / "repair-handoff.txt").read_bytes()
        self.assertEqual(first.returncode, 0)
        self.assertLessEqual(len(handoff), 8192)
        first_text = handoff
        self.assertTrue(self.checkpoint()["unresolvedTruncated"])
        self.assertEqual(self.execute().returncode, 0)
        self.assertEqual(first_text, (self.output / "repair-handoff.txt").read_bytes())

    def test_no_monetary_or_execution_fields(self):
        for key in ("cost", "command", "write", "subprocess"):
            self.write_ledger([], **{key: 1})
            self.assertNotEqual(self.execute().returncode, 0, key)


if __name__ == "__main__":
    unittest.main()
