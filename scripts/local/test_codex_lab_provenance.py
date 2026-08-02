import importlib.util
import json
import os
import subprocess
import sys
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
            verified = PROVENANCE.verify_binary(repo, source)
            self.assertEqual("current", verified["status"])
            self.assertIn("binary_sha256", verified)
            rejected = PROVENANCE.stage_candidate(repo, source, root / "artifacts")
            self.assertEqual("unverifiable", rejected["status"])

    def test_stage_publishes_read_only_content_addressed_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            repo, commit = self.make_repo(root)
            source = root / "source-codex-lab"
            write_fake_binary(source, commit, "clean")
            orphan = root / "artifacts" / "dogfood" / "candidates" / ".staging-old"
            orphan.mkdir(parents=True)
            (orphan / "partial").write_text("partial\n", encoding="utf-8")
            report = PROVENANCE.stage_candidate(repo, source, root / "artifacts")
            candidate = Path(report["binary_path"])

            self.assertEqual("current", report["status"])
            self.assertFalse(orphan.exists())
            os.utime(candidate.parent, ns=(1, 1))
            cached = PROVENANCE.stage_candidate(repo, source, root / "artifacts")
            self.assertEqual(candidate, Path(cached["binary_path"]))
            self.assertGreater(candidate.parent.stat().st_mtime_ns, 1)
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

    def test_stage_publishes_companion_in_bundle_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            repo, commit = self.make_repo(root)
            source = root / "source-codex-lab"
            write_fake_binary(source, commit, "clean")
            companion = root / "codex-code-mode-host"
            companion.write_text("host-v1\n", encoding="utf-8")
            companion.chmod(0o755)

            report = PROVENANCE.stage_candidate(
                repo, source, root / "artifacts", [companion]
            )
            candidate = Path(report["binary_path"])
            staged_companion = candidate.parent / companion.name

            self.assertEqual("current", report["status"])
            self.assertEqual(candidate.parent.name, report["bundle_sha256"])
            self.assertFalse(candidate.parent.stat().st_mode & 0o222)
            self.assertEqual(
                PROVENANCE.sha256_file(staged_companion),
                report["companion_sha256"][companion.name],
            )
            self.assertFalse(staged_companion.stat().st_mode & 0o222)

            cached = PROVENANCE.stage_candidate(
                repo, source, root / "artifacts", [companion]
            )
            self.assertEqual(candidate, Path(cached["binary_path"]))

            staged_companion.chmod(0o755)
            writable = PROVENANCE.stage_candidate(
                repo, source, root / "artifacts", [companion]
            )
            self.assertEqual("unverifiable", writable["status"])

            companion.write_text("host-v2\n", encoding="utf-8")
            replacement = PROVENANCE.stage_candidate(
                repo, source, root / "artifacts", [companion]
            )
            self.assertNotEqual(
                candidate.parent, Path(replacement["binary_path"]).parent
            )

    def test_prunes_old_candidates_and_preserves_active_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            candidate_root = Path(temp_dir) / "candidates"
            candidate_root.mkdir()
            candidates: list[Path] = []
            for index in range(PROVENANCE.MAX_STAGED_CANDIDATES + 2):
                commit = f"{index + 1:040x}"
                digest = f"{index + 1:064x}"
                candidate = candidate_root / commit / "clean" / digest
                candidate.mkdir(parents=True)
                binary = candidate / "codex-lab"
                binary.write_text("candidate\n", encoding="utf-8")
                binary.chmod(0o555)
                os.utime(candidate, ns=(index + 1, index + 1))
                candidates.append(candidate)
            unknown = candidate_root / "operator-notes"
            unknown.mkdir()
            linked = candidate_root / ("f" * 40)
            linked.symlink_to(unknown, target_is_directory=True)

            active_candidate = candidates[0]
            PROVENANCE.prune_staged_candidates(candidate_root, active_candidate)

            retained = PROVENANCE.staged_candidate_directories(candidate_root)
            self.assertEqual(PROVENANCE.MAX_STAGED_CANDIDATES, len(retained))
            self.assertIn(active_candidate, retained)
            self.assertFalse(candidates[1].exists())
            self.assertFalse(candidates[2].exists())
            self.assertTrue(all(candidate.exists() for candidate in candidates[3:]))
            self.assertTrue(unknown.is_dir())
            self.assertTrue(linked.is_symlink())

    def test_rejects_symlinked_orphaned_staging_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            candidate_root = root / "candidates"
            candidate_root.mkdir()
            escaped = root / "escaped"
            escaped.mkdir()
            staging_link = candidate_root / ".staging-link"
            staging_link.symlink_to(escaped, target_is_directory=True)

            with self.assertRaisesRegex(
                PROVENANCE.ProvenanceError,
                "staging path must be an owner-controlled directory",
            ):
                PROVENANCE.reap_orphaned_staging_directories(candidate_root)
            self.assertTrue(escaped.is_dir())
            self.assertTrue(staging_link.is_symlink())

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

    def test_verify_only_cli_emits_current_and_stale_json(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            repo, commit = self.make_repo(root)
            current = root / "current-codex-lab"
            write_fake_binary(current, commit, "clean")
            current_result = subprocess.run(
                [
                    sys.executable,
                    str(MODULE_PATH),
                    "--repo-root",
                    str(repo),
                    "--binary",
                    str(current),
                    "--verify-only",
                    "--json",
                ],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            current_report = json.loads(current_result.stdout)
            self.assertEqual(0, current_result.returncode)
            self.assertEqual("current", current_report["status"])
            self.assertIn("binary_sha256", current_report)

            stale = root / "stale-codex-lab"
            write_fake_binary(stale, "0" * 40, "clean")
            stale_result = subprocess.run(
                [
                    sys.executable,
                    str(MODULE_PATH),
                    "--repo-root",
                    str(repo),
                    "--binary",
                    str(stale),
                    "--verify-only",
                    "--json",
                ],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            stale_report = json.loads(stale_result.stdout)
            self.assertEqual(1, stale_result.returncode)
            self.assertEqual("stale", stale_report["status"])


if __name__ == "__main__":
    unittest.main()
