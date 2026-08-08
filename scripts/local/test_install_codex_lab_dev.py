import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from textwrap import dedent


REPO_ROOT = Path(__file__).resolve().parents[2]
INSTALLER = REPO_ROOT / "scripts" / "local" / "install-codex-lab-dev.sh"


class InstallCodexLabDevTest(unittest.TestCase):
    def create_fake_checkout(self, root: Path) -> tuple[Path, Path, dict[str, str]]:
        checkout = root / "checkout with spaces"
        for relative_path in (
            "scripts/local/cargo-build-env.sh",
            "scripts/local/codex_lab_provenance.py",
            "scripts/local/install-codex-lab-dev.sh",
        ):
            destination = checkout / relative_path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPO_ROOT / relative_path, destination)
        (checkout / "codex-rs").mkdir()
        (checkout / "codex-rs" / ".keep").touch()
        subprocess.run(["git", "init", "-q"], cwd=checkout, check=True)
        subprocess.run(
            ["git", "config", "user.name", "Codex Lab Test"],
            cwd=checkout,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.email", "codex-lab@example.invalid"],
            cwd=checkout,
            check=True,
        )
        source_state = checkout / "source-state.txt"
        source_state.write_text("base\n", encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=checkout, check=True)
        subprocess.run(["git", "commit", "-q", "-m", "base"], cwd=checkout, check=True)
        source_state.write_text("candidate\n", encoding="utf-8")
        subprocess.run(["git", "add", "source-state.txt"], cwd=checkout, check=True)
        subprocess.run(
            ["git", "commit", "-q", "-m", "candidate"], cwd=checkout, check=True
        )
        source_commit = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=checkout, text=True
        ).strip()

        fake_bin = root / "fake bin"
        fake_bin.mkdir()
        (fake_bin / "bash").symlink_to("/bin/bash")
        fake_cargo = fake_bin / "cargo"
        fake_cargo.write_text(
            dedent(
                """\
                #!/bin/sh
                set -eu
                printf 'build\\n' >>"$FAKE_CARGO_LOG"
                printf '%s\\n' "$PWD" >"$FAKE_CARGO_PWD"
                mkdir -p "$CARGO_TARGET_DIR/debug"
                cat >"$CARGO_TARGET_DIR/debug/codex-lab" <<'EOF'
                #!/usr/bin/env python3
                import json
                import os
                import sys
                from pathlib import Path

                executable = str(Path(__file__).resolve())
                if sys.argv[1:] == ["debug", "provenance", "--json"]:
                    print(json.dumps({
                        "schema_version": 1,
                        "version": "test",
                        "source_commit": os.environ["FAKE_BINARY_COMMIT"],
                        "dirty_state": os.environ["FAKE_DIRTY_STATE"],
                        "build_profile": "debug",
                        "build_channel": "dev",
                        "executable_path": executable,
                    }))
                else:
                    print(f"candidate={executable}")
                    if warning := os.environ.get("CODEX_LAB_PINNED_CANDIDATE_WARNING"):
                        print(f"startup_warning={warning}")
                EOF
                chmod +x "$CARGO_TARGET_DIR/debug/codex-lab"
                cat >"$CARGO_TARGET_DIR/debug/codex-code-mode-host" <<'EOF'
                #!/bin/sh
                exit 0
                EOF
                chmod +x "$CARGO_TARGET_DIR/debug/codex-code-mode-host"
                """
            ),
            encoding="utf-8",
        )
        fake_cargo.chmod(0o755)
        process_home = root / "process home"
        process_home.mkdir()
        environment = {
            **os.environ,
            "CODEX_LAB_CARGO_TARGET_DIR": str(root / "target with spaces"),
            "FAKE_BINARY_COMMIT": source_commit,
            "FAKE_CARGO_LOG": str(root / "cargo.log"),
            "FAKE_CARGO_PWD": str(root / "cargo.pwd"),
            "FAKE_DIRTY_STATE": "clean",
            "HOME": str(process_home),
            "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
        }
        return checkout, fake_cargo, environment

    def install_fake_candidate(
        self, root: Path
    ) -> tuple[Path, Path, Path, dict[str, str], subprocess.CompletedProcess[str]]:
        checkout, fake_cargo, environment = self.create_fake_checkout(root)
        bin_dir = root / "bin with spaces"
        lab_home = root / "home with spaces"
        caller_dir = root / "caller with spaces"
        caller_dir.mkdir()
        result = subprocess.run(
            [
                str(checkout / "scripts/local/install-codex-lab-dev.sh"),
                "--bin-dir",
                str(bin_dir),
                "--codex-lab-home",
                str(lab_home),
            ],
            cwd=caller_dir,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout)
        return checkout, fake_cargo, lab_home, environment, result

    def commit_source_state(self, checkout: Path, state: str) -> str:
        (checkout / "source-state.txt").write_text(f"{state}\n", encoding="utf-8")
        subprocess.run(["git", "add", "source-state.txt"], cwd=checkout, check=True)
        subprocess.run(["git", "commit", "-q", "-m", state], cwd=checkout, check=True)
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=checkout, text=True
        ).strip()

    def create_evidence_worktree(self, root: Path, checkout: Path) -> Path:
        evidence = root / "newer's evidence"
        subprocess.run(["git", "branch", "evidence"], cwd=checkout, check=True)
        subprocess.run(
            ["git", "worktree", "add", "-q", str(evidence), "evidence"],
            cwd=checkout,
            check=True,
        )
        return evidence

    def launch(
        self,
        launcher: Path,
        cwd: Path,
        environment: dict[str, str],
        *arguments: str,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(launcher), *arguments],
            cwd=cwd,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )

    def refresh_command(self, launch: subprocess.CompletedProcess[str]) -> str:
        return launch.stderr.split("Refresh command: ", maxsplit=1)[1].strip()

    def assert_no_refresh_command(
        self, launch: subprocess.CompletedProcess[str]
    ) -> None:
        self.assertNotIn("Refresh command:", launch.stderr)
        self.assertNotIn("install-codex-lab-dev.sh' --bin-dir", launch.stderr)

    def test_installs_launcher_pinned_to_staged_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, fake_cargo, lab_home, environment, install = (
                self.install_fake_candidate(root)
            )
            launcher = root / "bin with spaces" / "codex-lab"

            self.assertTrue(launcher.is_file())
            self.assertTrue(launcher.stat().st_mode & 0o111)
            contents = launcher.read_text(encoding="utf-8")
            self.assertIn("codex-lab-dev-shim", contents)
            self.assertIn("CODEX_LAB_HOME", contents)
            self.assertNotIn("CODEX_HOME", contents)
            self.assertIn(str(lab_home), contents)
            self.assertIn(str((lab_home / "working" / "dogfood").resolve()), contents)
            self.assertIn(str(checkout), contents)
            self.assertNotIn("cargo", contents)
            self.assertNotIn("python", contents.lower())
            self.assertIn("SOURCE_REPOSITORY_ID", contents)
            self.assertIn("CODEX_LAB_PINNED_CANDIDATE_WARNING", contents)
            self.assertIn("Installed Codex Lab dev launcher", install.stdout)
            self.assertIn("Pinned dogfood candidate", install.stdout)
            self.assertEqual(
                (root / "cargo.log").read_text(encoding="utf-8"), "build\n"
            )
            self.assertEqual(
                Path(
                    (root / "cargo.pwd").read_text(encoding="utf-8").strip()
                ).resolve(),
                (checkout / "codex-rs").resolve(),
            )

            fake_cargo.unlink()
            shutil.rmtree(checkout)
            runtime_home = root / "runtime-home"
            caller_dir = root / "runtime caller"
            caller_dir.mkdir()
            launch = subprocess.run(
                [str(launcher)],
                cwd=caller_dir,
                env={
                    "CODEX_LAB_HOME": str(runtime_home),
                    "HOME": environment["HOME"],
                    "PATH": os.environ["PATH"],
                },
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("candidate=", launch.stdout)
            self.assertIn("Pinned Codex Lab candidate:", launch.stderr)
            self.assertIn("recorded source checkout is unavailable", launch.stderr)
            self.assertNotIn("Refresh command:", launch.stderr)
            self.assertTrue(runtime_home.is_dir())
            self.assertEqual(
                (root / "cargo.log").read_text(encoding="utf-8"), "build\n"
            )
            candidate = Path(
                launch.stdout.split("candidate=", maxsplit=1)[1].splitlines()[0]
            )
            companion = candidate.parent / "codex-code-mode-host"
            self.assertTrue(companion.is_file())
            self.assertFalse(companion.stat().st_mode & 0o222)

    def test_executes_printed_refresh_for_clean_newer_source_and_clears_warning(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            evidence = self.create_evidence_worktree(root, checkout)
            newer_commit = self.commit_source_state(evidence, "newer")
            relative_evidence = os.path.relpath(evidence, root)

            launch = self.launch(launcher, root, environment, "-C", relative_evidence)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn(" is older than clean local source ", launch.stderr)
            self.assertIn(newer_commit, launch.stderr)
            refresh_command = self.refresh_command(launch)
            self.assertIn("newer'\"'\"'s evidence", refresh_command)
            refreshed_environment = {
                **environment,
                "FAKE_BINARY_COMMIT": newer_commit,
            }
            refresh = subprocess.run(
                ["/bin/sh", "-c", refresh_command],
                cwd=root,
                env=refreshed_environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(refresh.returncode, 0, refresh.stderr)
            self.assertIn("--profile 'dev'", launch.stderr)
            self.assertIn("startup_warning=Pinned Codex Lab candidate", launch.stdout)

            current = self.launch(
                launcher,
                root,
                refreshed_environment,
                f"--cd={relative_evidence}",
            )
            self.assertEqual(current.returncode, 0, current.stderr)
            self.assertIn(f"Pinned Codex Lab candidate: {newer_commit}", current.stderr)
            self.assertNotIn("warning:", current.stderr)

            passthrough = self.launch(
                launcher, root, refreshed_environment, "--", "-C", str(checkout)
            )
            self.assertEqual(passthrough.returncode, 0, passthrough.stderr)
            self.assertNotIn("warning:", passthrough.stderr)

    def test_warns_when_candidate_is_newer_than_source_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            subprocess.run(
                ["git", "checkout", "-q", "--detach", "HEAD^"],
                cwd=checkout,
                check=True,
            )
            older_commit = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=checkout, text=True
            ).strip()

            launch = subprocess.run(
                [str(launcher)],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn(" is ahead of clean local source ", launch.stderr)
            self.assertIn(older_commit, launch.stderr)
            self.assertIn("would downgrade the candidate", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_warns_when_source_checkout_diverged(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            subprocess.run(
                ["git", "checkout", "-q", "-b", "diverged", "HEAD^"],
                cwd=checkout,
                check=True,
            )
            (checkout / "source-state.txt").write_text("diverged\n", encoding="utf-8")
            subprocess.run(["git", "add", "source-state.txt"], cwd=checkout, check=True)
            subprocess.run(
                ["git", "commit", "-q", "-m", "diverged"],
                cwd=checkout,
                check=True,
            )

            launch = subprocess.run(
                [str(launcher)],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("have diverged", launch.stderr)
            self.assertIn(
                "Select a clean checkout from the intended history", launch.stderr
            )
            self.assert_no_refresh_command(launch)

    def test_warns_when_matching_source_checkout_is_dirty(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            (checkout / "source-state.txt").write_text("dirty\n", encoding="utf-8")

            launch = subprocess.run(
                [str(launcher)],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("Local source", launch.stderr)
            self.assertIn("is dirty", launch.stderr)
            self.assertIn("Commit or stash the changes", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_dirty_source_at_different_commit_does_not_offer_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            newer_commit = self.commit_source_state(checkout, "newer")
            (checkout / "source-state.txt").write_text("dirty\n", encoding="utf-8")

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn(f"Local source {newer_commit} is dirty", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_incomparable_rewritten_history_does_not_offer_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            candidate_commit = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=checkout, text=True
            ).strip()
            subprocess.run(
                ["git", "checkout", "-q", "--orphan", "rewritten"],
                cwd=checkout,
                check=True,
            )
            subprocess.run(
                ["git", "commit", "-q", "-m", "rewritten"],
                cwd=checkout,
                check=True,
            )
            subprocess.run(["git", "branch", "-D", "master"], cwd=checkout, check=True)
            subprocess.run(
                ["git", "reflog", "expire", "--expire=now", "--all"],
                cwd=checkout,
                check=True,
            )
            subprocess.run(["git", "gc", "--prune=now"], cwd=checkout, check=True)
            missing = subprocess.run(
                ["git", "cat-file", "-e", f"{candidate_commit}^{{commit}}"],
                cwd=checkout,
                capture_output=True,
                check=False,
            )
            self.assertNotEqual(missing.returncode, 0)

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("cannot be compared with clean local source", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_unreadable_unborn_head_does_not_offer_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            subprocess.run(
                ["git", "checkout", "-q", "--orphan", "unborn"],
                cwd=checkout,
                check=True,
            )

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("commit or clean status could not be read", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_missing_installer_does_not_offer_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            (checkout / "scripts/local/install-codex-lab-dev.sh").unlink()
            subprocess.run(
                ["git", "add", "scripts/local/install-codex-lab-dev.sh"],
                cwd=checkout,
                check=True,
            )
            subprocess.run(
                ["git", "commit", "-q", "-m", "remove installer"],
                cwd=checkout,
                check=True,
            )

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn(
                "does not contain scripts/local/install-codex-lab-dev.sh", launch.stderr
            )
            self.assert_no_refresh_command(launch)

    def test_non_executable_installer_does_not_offer_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            installer = checkout / "scripts/local/install-codex-lab-dev.sh"
            installer.chmod(0o644)
            subprocess.run(
                ["git", "add", "scripts/local/install-codex-lab-dev.sh"],
                cwd=checkout,
                check=True,
            )
            subprocess.run(
                ["git", "commit", "-q", "-m", "disable installer"],
                cwd=checkout,
                check=True,
            )

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("is not executable there", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_untracked_installer_replacement_does_not_offer_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            installer = checkout / "scripts/local/install-codex-lab-dev.sh"
            installer.unlink()
            subprocess.run(
                ["git", "add", "scripts/local/install-codex-lab-dev.sh"],
                cwd=checkout,
                check=True,
            )
            subprocess.run(
                ["git", "commit", "-q", "-m", "remove installer"],
                cwd=checkout,
                check=True,
            )
            installer.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            installer.chmod(0o755)

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("is not tracked at that source commit", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_directory_at_installer_path_does_not_offer_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            installer = checkout / "scripts/local/install-codex-lab-dev.sh"
            installer.unlink()
            subprocess.run(
                ["git", "add", "scripts/local/install-codex-lab-dev.sh"],
                cwd=checkout,
                check=True,
            )
            subprocess.run(
                ["git", "commit", "-q", "-m", "remove installer"],
                cwd=checkout,
                check=True,
            )
            installer.mkdir()

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("is not a regular file there", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_sparse_checkout_does_not_offer_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            evidence = self.create_evidence_worktree(root, checkout)
            self.commit_source_state(evidence, "newer")
            subprocess.run(
                [
                    "git",
                    "sparse-checkout",
                    "set",
                    "--no-cone",
                    "/scripts/local/",
                    "/source-state.txt",
                ],
                cwd=evidence,
                check=True,
            )

            launch = self.launch(launcher, root, environment, "-C", str(evidence))

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("is a sparse checkout", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_launches_without_git(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            _, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            runtime_bin = root / "runtime bin"
            runtime_bin.mkdir()
            for command in ("mkdir", "python3"):
                executable = shutil.which(command)
                self.assertIsNotNone(executable)
                (runtime_bin / command).symlink_to(executable)

            launch = self.launch(
                launcher,
                root,
                {**environment, "PATH": str(runtime_bin)},
            )

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("Git is not on PATH", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_launches_when_recorded_source_checkout_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            shutil.rmtree(checkout)

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("recorded source checkout is unavailable", launch.stderr)
            self.assert_no_refresh_command(launch)

    def test_launches_when_recorded_checkout_is_not_a_repository(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            shutil.rmtree(checkout / ".git")

            launch = self.launch(launcher, root, environment)

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn(
                "No same-repository source checkout is available", launch.stderr
            )
            self.assert_no_refresh_command(launch)

    def test_ignores_unrelated_checkout_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            unrelated = root / "unrelated"
            unrelated.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=unrelated, check=True)
            subprocess.run(
                ["git", "config", "user.name", "Unrelated"],
                cwd=unrelated,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.email", "unrelated@example.invalid"],
                cwd=unrelated,
                check=True,
            )
            (unrelated / "file").write_text("unrelated\n", encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=unrelated, check=True)
            subprocess.run(
                ["git", "commit", "-q", "-m", "unrelated"],
                cwd=unrelated,
                check=True,
            )

            launch = subprocess.run(
                [str(launcher), "-C", str(unrelated)],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertNotIn("warning:", launch.stderr)
            self.assertNotIn(str(unrelated), launch.stderr)

    def test_ignores_cached_origin_main_when_checkout_head_is_current(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, _, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            tree = subprocess.check_output(
                ["git", "rev-parse", "HEAD^{tree}"], cwd=checkout, text=True
            ).strip()
            cached_remote_commit = subprocess.run(
                ["git", "commit-tree", tree, "-p", "HEAD"],
                cwd=checkout,
                input="cached remote\n",
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
            subprocess.run(
                ["git", "update-ref", "refs/remotes/origin/main", cached_remote_commit],
                cwd=checkout,
                check=True,
            )

            launch = subprocess.run(
                [str(launcher)],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertNotIn("warning:", launch.stderr)
            self.assertNotIn(cached_remote_commit, launch.stderr)

    def test_failed_reinstall_preserves_existing_launcher(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, lab_home, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            original_contents = launcher.read_text(encoding="utf-8")

            failed = subprocess.run(
                [
                    str(checkout / "scripts/local/install-codex-lab-dev.sh"),
                    "--bin-dir",
                    str(launcher.parent),
                    "--codex-lab-home",
                    str(lab_home),
                ],
                env={**environment, "FAKE_BINARY_COMMIT": "0" * 40},
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("provenance stale", failed.stderr)
            self.assertEqual(launcher.read_text(encoding="utf-8"), original_contents)

    def test_refuses_to_replace_unmanaged_launcher_without_force(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            launcher = bin_dir / "codex-lab"
            launcher.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")

            result = subprocess.run(
                [
                    str(INSTALLER),
                    "--bin-dir",
                    str(bin_dir),
                    "--codex-lab-home",
                    str(root / "home"),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("refusing to replace non-managed launcher", result.stdout)

    def test_requires_supported_python(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            fake_bin = Path(temp_dir_name) / "bin"
            fake_bin.mkdir()
            fake_python = fake_bin / "python3"
            fake_python.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
            fake_python.chmod(0o755)

            result = subprocess.run(
                [str(INSTALLER), "--bin-dir", str(Path(temp_dir_name) / "output")],
                env={
                    **os.environ,
                    "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
                },
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Python 3.10 or newer is required", result.stdout)

    def test_reports_missing_option_value(self) -> None:
        result = subprocess.run(
            [str(INSTALLER), "--bin-dir"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )

        self.assertEqual(result.returncode, 2)
        self.assertIn("--bin-dir requires a directory", result.stdout)


if __name__ == "__main__":
    unittest.main()
