import importlib.util
import json
import math
import os
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "busy_host_baseline", Path(__file__).with_name("busy_host_baseline.py")
)
assert SPEC and SPEC.loader
busy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(busy)


def fixture(
    root: Path, *, quality: bool = True, mismatch: bool = False
) -> tuple[Path, Path]:
    evidence_root = root / "evidence"
    evidence_root.mkdir()
    attempts = []
    for replica, duration in (("r1", 100), ("r2", 120)):
        name = f"pair-01-{replica}.json"
        expected_diff = "d" if not mismatch or replica == "r1" else "x"
        record = {
            "schemaVersion": 4,
            "exitCode": 0,
            "commandStatus": "completed",
            "scenario": "warm-edit",
            "source": {"commit": "c"},
            "sourceEdit": {"expectedDiffSha256": expected_diff},
            "buildContext": {
                "configuration": "dev",
                "invocationFingerprint": "i",
                "sourcePathFingerprint": "s",
                "environmentFingerprint": replica,
                "fileFingerprints": {"lockfile": "l", "toolchain": "t"},
            },
            "measurementQuality": {"matchedAnalysisEligible": quality},
            "phaseDurationsMs": {"command": duration},
        }
        (evidence_root / name).write_text(json.dumps(record), encoding="utf-8")
        attempts.append(
            {
                "pair": 1,
                "replica": replica,
                "orderIndex": 0 if replica == "r1" else 1,
                "expectedDiffSha256": expected_diff,
                "evidence": name,
                "harnessExitCode": 0,
            }
        )
    manifest = root / "manifest.json"
    manifest.write_text(
        json.dumps({"sourceCommit": "c", "attempts": attempts}), encoding="utf-8"
    )
    return manifest, evidence_root


def args_for(root: Path, manifest: Path, evidence: Path):
    return busy.parse_args(
        f"--manifest {manifest} --evidence-root {evidence} --output {root / 'out.json'} "
        "--lane aa --configuration dev".split()
    )


def analyzed(root: Path, **options):
    manifest, evidence = fixture(root, **options)
    return busy.analyze(args_for(root, manifest, evidence))


class BusyHostAnalysisTest(unittest.TestCase):
    def test_pairs_are_oriented_and_summarized(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = analyzed(root)
        summary = result["summary"]
        self.assertEqual(summary["eligiblePairCount"], 1)
        self.assertEqual(summary["failureCount"], 0)
        self.assertAlmostEqual(
            summary["pairedLogRatios"][0]["logRatioReplica0OverReplica1"],
            math.log(100 / 120),
        )

    def test_quality_failure_is_retained_and_not_paired(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = analyzed(root, quality=False)
        self.assertEqual(result["summary"]["failureCount"], 2)
        self.assertEqual(result["summary"]["eligiblePairCount"], 0)
        self.assertEqual(
            result["attempts"][0]["failure"], "matched-analysis-ineligible"
        )

    def test_identity_mismatch_is_preserved(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = analyzed(root, mismatch=True)
        self.assertEqual(result["summary"]["completePairCount"], 1)
        self.assertEqual(result["summary"]["eligiblePairCount"], 0)
        self.assertEqual(result["attempts"][0]["failure"], "pair-identity-mismatch")

    def test_traversal_and_symlink_inputs_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, evidence = fixture(root)
            data = json.loads(manifest.read_text())
            data["attempts"][0]["evidence"] = "../outside.json"
            manifest.write_text(json.dumps(data))
            args = args_for(root, manifest, evidence)
            with self.assertRaises(busy.BaselineError):
                busy.analyze(args)
            data["attempts"][0]["evidence"] = "linked.json"
            (root / "outside.json").write_text("{}")
            (evidence / "linked.json").symlink_to(root / "outside.json")
            manifest.write_text(json.dumps(data))
            with self.assertRaises(busy.BaselineError):
                busy.analyze(args_for(root, manifest, evidence))

    def test_missing_evidence_is_retained_as_failed_attempt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, evidence = fixture(root)
            data = json.loads(manifest.read_text())
            data["attempts"][0]["evidence"] = "missing.json"
            manifest.write_text(json.dumps(data))
            result = busy.analyze(args_for(root, manifest, evidence))
        self.assertEqual(result["attempts"][0]["failure"], "evidence-unavailable")
        self.assertEqual(result["summary"]["failureCount"], 1)

    def test_nonfinite_duration_and_missing_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, evidence = fixture(root)
            path = evidence / "pair-01-r1.json"
            record = json.loads(path.read_text())
            record["phaseDurationsMs"]["command"] = float("nan")
            path.write_text(json.dumps(record))
            result = busy.analyze(args_for(root, manifest, evidence))
            self.assertEqual(
                result["attempts"][0]["failure"], "command-duration-missing"
            )
            record["phaseDurationsMs"] = None
            path.write_text(json.dumps(record))
            result = busy.analyze(args_for(root, manifest, evidence))
            self.assertEqual(result["attempts"][0]["durationMs"], None)
            record["phaseDurationsMs"] = {"command": 100}
            record["buildContext"]["invocationFingerprint"] = None
            path.write_text(json.dumps(record))
            with self.assertRaises(busy.BaselineError):
                busy.analyze(args_for(root, manifest, evidence))

    def test_duplicate_pair_replica_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, evidence = fixture(root)
            data = json.loads(manifest.read_text())
            data["attempts"].append(data["attempts"][0])
            manifest.write_text(json.dumps(data))
            with self.assertRaises(busy.BaselineError):
                busy.analyze(args_for(root, manifest, evidence))

    def test_environment_drift_invalidates_all_replica_attempts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, evidence = fixture(root)
            data = json.loads(manifest.read_text())
            for replica in ("r1", "r2"):
                name = f"pair-02-{replica}.json"
                record = json.loads((evidence / f"pair-01-{replica}.json").read_text())
                record["buildContext"]["environmentFingerprint"] = (
                    "r2-drift" if replica == "r2" else replica
                )
                (evidence / name).write_text(json.dumps(record))
                item = next(
                    x for x in data["attempts"] if x["replica"] == replica
                ).copy()
                item.update(pair=2, evidence=name)
                data["attempts"].append(item)
            manifest.write_text(json.dumps(data))
            result = busy.analyze(args_for(root, manifest, evidence))
        self.assertEqual(result["summary"]["eligiblePairCount"], 0)
        self.assertTrue(
            all(
                not item["eligible"]
                for item in result["attempts"]
                if item["replica"] == "r2"
            )
        )

    def test_fifo_evidence_is_retained_without_blocking(self) -> None:
        if not hasattr(os, "mkfifo"):
            self.skipTest("FIFO unavailable")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, evidence = fixture(root)
            fifo = evidence / "fifo.json"
            os.mkfifo(fifo)
            data = json.loads(manifest.read_text())
            data["attempts"][0]["evidence"] = "fifo.json"
            manifest.write_text(json.dumps(data))
            result = busy.analyze(args_for(root, manifest, evidence))
        self.assertEqual(result["attempts"][0]["failure"], "evidence-unavailable")

    def test_output_must_be_new(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, evidence = fixture(root)
            output = root / "out.json"
            output.write_text("old")
            code = busy.main(
                f"--manifest {manifest} --evidence-root {evidence} --output {output} "
                "--lane aa --configuration dev".split()
            )
        self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
