import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("codex_lab_provenance.py")
SPEC = importlib.util.spec_from_file_location("codex_lab_provenance", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
PROVENANCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROVENANCE)


def run(*command: str, cwd: Path) -> str:
    return subprocess.check_output(command, cwd=cwd, text=True).strip()


def write_fake_binary(path: Path, commit: str, dirty_state: str) -> None:
    path.write_text(
        f"""#!/usr/bin/env python3
import json
import sys
from pathlib import Path
executable = str(Path(__file__).resolve())
if sys.argv[1:] == ["debug", "provenance", "--json"]:
    print(json.dumps({{
        "schema_version": 1,
        "version": "test",
        "source_commit": "{commit}",
        "dirty_state": "{dirty_state}",
        "build_profile": "debug",
        "build_channel": "dev",
        "executable_path": executable,
    }}))
else:
    print(executable)
""",
        encoding="utf-8",
    )
    path.chmod(0o755)


class CodexLabProvenanceTest(unittest.TestCase):
    def make_repo(self, root: Path) -> tuple[Path, str]:
        repo = root / "repo"
        repo.mkdir()
        run("git", "init", "-q", cwd=repo)
        run("git", "config", "user.name", "Codex Lab Test", cwd=repo)
        run("git", "config", "user.email", "codex-lab@example.invalid", cwd=repo)
        (repo / "tracked.txt").write_text("tracked\n", encoding="utf-8")
        run("git", "add", "tracked.txt", cwd=repo)
        run("git", "commit", "-q", "-m", "test", cwd=repo)
        return repo, run("git", "rev-parse", "HEAD", cwd=repo)

    def test_checkout_identity_ignores_untracked_files(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            repo, commit = self.make_repo(root)
            (repo / "untracked.txt").write_text("untracked\n", encoding="utf-8")
            self.assertEqual(
                {"source_commit": commit, "dirty_state": "clean"},
                PROVENANCE.checkout_identity(repo),
            )
            (repo / "tracked.txt").write_text("changed\n", encoding="utf-8")
            self.assertEqual("dirty", PROVENANCE.checkout_identity(repo)["dirty_state"])
            source = root / "dirty-codex-lab"
            write_fake_binary(source, commit, "dirty")
            rejected = PROVENANCE.stage_candidate(repo, source, root / "artifacts")
            self.assertEqual("unverifiable", rejected["status"])

    def test_stage_publishes_read_only_content_addressed_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            repo, commit = self.make_repo(root)
            source = root / "source-codex-lab"
            write_fake_binary(source, commit, "clean")
            report = PROVENANCE.stage_candidate(repo, source, root / "artifacts")
            candidate = Path(report["binary_path"])

            self.assertEqual("current", report["status"])
            cached = PROVENANCE.stage_candidate(repo, source, root / "artifacts")
            self.assertEqual(candidate, Path(cached["binary_path"]))
            candidate.chmod(0o755)
            writable = PROVENANCE.stage_candidate(repo, source, root / "artifacts")
            self.assertEqual("unverifiable", writable["status"])
            shared = root / "shared"
            shared.mkdir(mode=0o777)
            shared.chmod(0o777)
            with self.assertRaises(PROVENANCE.ProvenanceError):
                PROVENANCE.stage_candidate(repo, candidate, shared / "artifacts")

            escaped = root / "escaped"
            escaped.mkdir()
            linked_root = root / "linked" / "dogfood" / "candidates"
            linked_root.mkdir(parents=True)
            (linked_root / commit).symlink_to(escaped, target_is_directory=True)
            with self.assertRaises(PROVENANCE.ProvenanceError):
                PROVENANCE.stage_candidate(repo, candidate, root / "linked")

    def test_verifier_distinguishes_stale_and_unverifiable(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            repo, commit = self.make_repo(root)
            stale = root / "stale"
            write_fake_binary(stale, "0" * 40, "clean")
            invalid = root / "invalid"
            invalid.write_text("#!/bin/sh\nprintf 'not json\\n'\n", encoding="utf-8")
            invalid.chmod(0o755)

            report = PROVENANCE.stage_candidate(repo, stale, root / "stale-artifacts")
            self.assertEqual("stale", report["status"])
            with self.assertRaises(PROVENANCE.ProvenanceError):
                PROVENANCE.stage_candidate(repo, invalid, root / "invalid-artifacts")
            provenance = PROVENANCE.reported_provenance(stale)
            provenance["source_commit"] = commit
            for executable_path in (str(root / "other"), "bad\0path"):
                provenance["executable_path"] = executable_path
                report = PROVENANCE.verification_report(
                    PROVENANCE.checkout_identity(repo), stale, provenance
                )
                self.assertEqual("unverifiable", report["status"])
            provenance["executable_path"] = str(stale.resolve())
            for field, value in (
                ("schema_version", True),
                ("dirty_state", []),
            ):
                with self.subTest(field=field):
                    invalid_shape = PROVENANCE.verification_report(
                        PROVENANCE.checkout_identity(repo),
                        stale,
                        {**provenance, field: value},
                    )
                    self.assertEqual("unverifiable", invalid_shape["status"])


if __name__ == "__main__":
    unittest.main()
