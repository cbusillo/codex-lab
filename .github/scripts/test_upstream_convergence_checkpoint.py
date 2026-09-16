import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import upstream_convergence_checkpoint as checkpoint


def git(repo, *args):
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True, text=True
    ).stdout.strip()


class CheckpointFixtureTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        git(self.source, "init", "-b", "main")
        git(self.source, "config", "user.email", "fixture@example.com")
        git(self.source, "config", "user.name", "Fixture")
        for name in checkpoint.SOURCE_FILES:
            target = self.source / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(checkpoint.SOURCE_ROOT / name, target)
        (self.source / ".gitignore").write_text("__pycache__/\n")
        (self.source / "changed").write_text("before\n")
        (self.source / "deleted").write_text("deleted\n")
        (self.source / "executable").write_text("#!/bin/sh\n")
        git(self.source, "add", ".")
        git(self.source, "commit", "-m", "synthetic source and baseline")
        self.base = git(self.source, "rev-parse", "HEAD")
        self.repo = self.root / "candidate"
        git(
            self.source,
            "worktree",
            "add",
            "-b",
            "work/fixture",
            str(self.repo),
            self.base,
        )
        (self.repo / "changed").write_text("after\n")
        (self.repo / "deleted").unlink()
        (self.repo / "executable").chmod(0o755)
        for index in range(251):
            (self.repo / f"added-{index:03}").write_text(f"value {index}\n")
        (self.repo / "new\nline").write_text("newline path\n")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-m", "synthetic candidate")
        self.head = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "remote", "add", "openai", "https://github.com/openai/codex.git")
        git(self.repo, "update-ref", "refs/remotes/openai/main", self.base)
        self.config = self.root / "config.json"
        self.validation = self.root / "validation.json"
        self.output = self.root / "checkpoint.json"
        self.config.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "attemptId": "fixture-1",
                    "startedAt": "2026-09-16T00:00:00Z",
                    "settings": {"synthetic": True},
                }
            )
        )
        self.validation.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "attemptId": "fixture-1",
                    "candidate": self.head,
                    "checks": [
                        {
                            "name": "fixture",
                            "outcome": "not-run",
                            "artifactSha256": None,
                        }
                    ],
                }
            )
        )
        self.script = self.source / ".github/scripts/upstream_convergence_checkpoint.py"

    def execute(self, operation, sha=None, **overrides):
        values = {
            "repo": self.repo,
            "base": self.base,
            "local": self.base,
            "upstream": self.base,
            "candidate": self.head,
            "source": self.base,
            "config": self.config,
            "validation": self.validation,
            "checkpoint": self.output,
            **overrides,
        }
        if sha is not None:
            values["sha256"] = sha
        args = [sys.executable, str(self.script), operation]
        for key, value in values.items():
            args.extend([f"--{key}", str(value)])
        return subprocess.run(args, capture_output=True, text=True)

    def record(self):
        result = self.execute("record")
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_stop_and_resume_preserves_complete_ordered_inventory(self):
        receipt = self.record()
        result = self.execute("verify", receipt["sha256"])
        self.assertEqual(result.returncode, 0, result.stderr)
        document = json.loads(self.output.read_bytes())
        self.assertEqual(document["patch"]["pathTotal"], 255)
        by_path = {row["path"]: row for row in document["patch"]["paths"]}
        self.assertEqual(
            by_path["changed"],
            {
                "path": "changed",
                "oldBlob": git(self.repo, "rev-parse", f"{self.base}:changed"),
                "newBlob": git(self.repo, "rev-parse", f"{self.head}:changed"),
                "oldMode": "100644",
                "newMode": "100644",
                "status": "M",
            },
        )
        self.assertEqual(by_path["deleted"]["newMode"], "000000")
        self.assertEqual(by_path["executable"]["newMode"], "100755")
        self.assertIn("added-250", by_path)
        self.assertIn("new\nline", by_path)
        self.assertEqual(json.loads(result.stdout)["status"], "unchanged")
        self.assertEqual(git(self.repo, "status", "--porcelain"), "")

    def test_missing_or_reversed_rows_refused_even_with_recomputed_digest(self):
        receipt = self.record()
        original = self.output.read_bytes()
        for modification in ("omit", "reverse"):
            with self.subTest(modification=modification):
                document = json.loads(original)
                if modification == "omit":
                    document["patch"]["paths"].pop(201)
                    document["patch"]["pathTotal"] -= 1
                else:
                    for row in document["patch"]["paths"]:
                        row["oldBlob"], row["newBlob"] = row["newBlob"], row["oldBlob"]
                data = json.dumps(document).encode()
                self.output.write_bytes(data)
                self.assertNotEqual(
                    self.execute("verify", receipt["sha256"]).returncode, 0
                )
                result = self.execute("verify", hashlib.sha256(data).hexdigest())
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("ordered inventory changed", result.stderr)

    def test_changed_config_and_stale_validation_are_refused(self):
        receipt = self.record()
        original = self.config.read_bytes()
        config = json.loads(original)
        config["settings"]["different"] = True
        self.config.write_text(json.dumps(config))
        self.assertNotEqual(self.execute("verify", receipt["sha256"]).returncode, 0)
        self.config.write_bytes(original)
        validation = json.loads(self.validation.read_bytes())
        validation["candidate"] = self.base
        self.validation.write_text(json.dumps(validation))
        result = self.execute("verify", receipt["sha256"])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("validation evidence must name", result.stderr)

    def test_dirty_untracked_and_changed_candidate_refused(self):
        receipt = self.record()
        path = self.repo / "untracked"
        path.write_text("pending")
        self.assertNotEqual(self.execute("verify", receipt["sha256"]).returncode, 0)
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-m", "later candidate")
        self.assertNotEqual(self.execute("verify", receipt["sha256"]).returncode, 0)

    def test_changed_source_refused(self):
        receipt = self.record()
        with self.script.open("a") as handle:
            handle.write("\n# changed source\n")
        self.assertNotEqual(self.execute("verify", receipt["sha256"]).returncode, 0)

    def test_overwrite_and_worktree_artifacts_refused(self):
        receipt = self.record()
        self.assertNotEqual(self.execute("record").returncode, 0)
        self.assertEqual(
            hashlib.sha256(self.output.read_bytes()).hexdigest(), receipt["sha256"]
        )
        result = self.execute("record", checkpoint=self.repo / "checkpoint.json")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.repo / "checkpoint.json").exists())

    def test_wrong_remote_and_refs_refused(self):
        receipt = self.record()
        self.assertNotEqual(
            self.execute("verify", receipt["sha256"], base=self.head).returncode, 0
        )
        git(
            self.repo,
            "remote",
            "set-url",
            "openai",
            "https://github.com/other/fork.git",
        )
        self.assertNotEqual(self.execute("verify", receipt["sha256"]).returncode, 0)


if __name__ == "__main__":
    unittest.main()
