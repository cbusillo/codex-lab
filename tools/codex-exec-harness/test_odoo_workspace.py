#!/usr/bin/env python3
"""Regression tests for generated Odoo workspace launching."""

import importlib.util
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("odoo_workspace.py")
SPEC = importlib.util.spec_from_file_location("odoo_workspace", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
ODOO_WORKSPACE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ODOO_WORKSPACE
SPEC.loader.exec_module(ODOO_WORKSPACE)


class WorkspaceFixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.workspace = root / "workspace"
        self.workspace.mkdir()
        self.sources = self.workspace / "sources"
        self.sources.mkdir()
        self.tenant = root / "tenant"
        self.devkit = root / "devkit"
        self.tenant.mkdir()
        self.devkit.mkdir()
        (self.sources / "tenant").symlink_to(self.tenant, target_is_directory=True)
        (self.sources / "devkit").symlink_to(self.devkit, target_is_directory=True)
        self.runtime = self.sources / "runtime"
        self.runtime.mkdir()
        self.manifest = self.tenant / "workspace.toml"
        self.manifest.write_text("schema_version = 1\n", encoding="utf-8")
        self.agents = self.workspace / "AGENTS.md"
        self.agents.write_text("# Canonical Odoo workspace guide\n", encoding="utf-8")
        self.local_notes = self.workspace / "workspace.local.md"
        self.local_notes.write_text("local supplemental notes\n", encoding="utf-8")

    def payload(self) -> dict[str, object]:
        sources = [
            self._source("tenant", self.tenant, "linked_path", True),
            self._source("devkit", self.devkit, "linked_path", True),
            self._source("runtime", self.runtime, "managed_checkout", False),
        ]
        return {
            "schema_version": 1,
            "workspace_path": str(self.workspace),
            "workspace_exists": True,
            "current": True,
            "stale_reasons": [],
            "lock_file_exists": True,
            "lock_file_current": True,
            "manifest": {
                "path": str(self.manifest),
                "sha256": hashlib.sha256(self.manifest.read_bytes()).hexdigest(),
                "current": True,
            },
            "surface_current": True,
            "materialization_current": True,
            "source_baseline_current": True,
            "managed_source_baseline_current": True,
            "sources": sources,
            "edit_roots": [
                {
                    "role": source["role"],
                    "workspace_relative_path": source["workspace_relative_path"],
                    "resolved_path": source["resolved_path"],
                }
                for source in sources
                if source["editable"]
            ],
            "local_notes": {
                "path": str(self.local_notes),
                "exists": True,
                "valid": True,
                "semantics": "supplemental_non_secret_notes",
            },
            "reserved_override": {
                "path": str(self.workspace / "AGENTS.override.md"),
                "exists": False,
                "semantics": "full_replacement",
                "allowed_in_normal_flow": False,
            },
            "workspace_agents_path": str(self.agents),
            "workspace_agents_exists": True,
        }

    def _source(
        self,
        role: str,
        resolved_path: Path,
        materialization: str,
        editable: bool,
    ) -> dict[str, object]:
        relative_path = f"sources/{role}"
        return {
            "role": role,
            "workspace_relative_path": relative_path,
            "workspace_entry_path": str(self.workspace / relative_path),
            "resolved_path": str(resolved_path.resolve()),
            "actual_resolved_path": str(resolved_path.resolve()),
            "materialization": materialization,
            "materialization_state": "current",
            "materialization_current": True,
            "editable": editable,
        }


class OdooWorkspaceValidationTest(unittest.TestCase):
    def test_current_non_git_workspace_exposes_only_declared_edit_roots(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))

            launch = ODOO_WORKSPACE.validate_workspace_status(
                fixture.payload(),
                manifest_path=fixture.manifest,
                status_command=("uv", "workspace", "status", "--check"),
            )

            self.assertEqual(launch.workspace_path, fixture.workspace.resolve())
            self.assertIsNone(launch.git_root)
            self.assertEqual(
                launch.writable_roots,
                (fixture.tenant.resolve(), fixture.devkit.resolve()),
            )
            self.assertEqual([source.role for source in launch.managed_sources], ["runtime"])

    def test_reserved_override_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            payload = fixture.payload()
            payload["reserved_override"] = {
                "path": str(fixture.workspace / "AGENTS.override.md"),
                "exists": True,
                "semantics": "full_replacement",
                "allowed_in_normal_flow": False,
            }

            with self.assertRaisesRegex(ODOO_WORKSPACE.OdooWorkspaceError, "shadowed"):
                ODOO_WORKSPACE.validate_workspace_status(
                    payload,
                    manifest_path=fixture.manifest,
                    status_command=("uv",),
                )

    def test_editable_source_baseline_drift_does_not_block_current_workspace(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            payload = fixture.payload()
            payload["source_baseline_current"] = False

            launch = ODOO_WORKSPACE.validate_workspace_status(
                payload,
                manifest_path=fixture.manifest,
                status_command=("uv",),
            )

            self.assertEqual(
                launch.writable_roots,
                (fixture.tenant.resolve(), fixture.devkit.resolve()),
            )

    def test_redirected_workspace_entry_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            payload = fixture.payload()
            payload["sources"][0]["workspace_entry_path"] = str(fixture.tenant)

            with self.assertRaisesRegex(ODOO_WORKSPACE.OdooWorkspaceError, "redirected"):
                ODOO_WORKSPACE.validate_workspace_status(
                    payload,
                    manifest_path=fixture.manifest,
                    status_command=("uv",),
                )

    def test_redirected_local_notes_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            redirected_notes = fixture.root / "redirected-notes.md"
            redirected_notes.write_text("outside guidance\n", encoding="utf-8")
            fixture.local_notes.unlink()
            fixture.local_notes.symlink_to(redirected_notes)

            with self.assertRaisesRegex(ODOO_WORKSPACE.OdooWorkspaceError, "regular file"):
                ODOO_WORKSPACE.validate_workspace_status(
                    fixture.payload(),
                    manifest_path=fixture.manifest,
                    status_command=("uv",),
                )

    def test_managed_checkout_cannot_be_declared_editable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            payload = fixture.payload()
            payload["sources"][2]["editable"] = True
            payload["edit_roots"].append(
                {
                    "role": "runtime",
                    "workspace_relative_path": "sources/runtime",
                    "resolved_path": str(fixture.runtime),
                }
            )

            with self.assertRaisesRegex(ODOO_WORKSPACE.OdooWorkspaceError, "cannot be editable"):
                ODOO_WORKSPACE.validate_workspace_status(
                    payload,
                    manifest_path=fixture.manifest,
                    status_command=("uv",),
                )

    def test_editable_source_cannot_overlap_generated_workspace(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            payload = fixture.payload()
            tenant_link = fixture.sources / "tenant"
            tenant_link.unlink()
            tenant_link.symlink_to(fixture.root, target_is_directory=True)
            redirected_root = str(fixture.root.resolve())
            payload["sources"][0]["resolved_path"] = redirected_root
            payload["sources"][0]["actual_resolved_path"] = redirected_root
            payload["edit_roots"][0]["resolved_path"] = redirected_root

            with self.assertRaisesRegex(ODOO_WORKSPACE.OdooWorkspaceError, "generated workspace"):
                ODOO_WORKSPACE.validate_workspace_status(
                    payload,
                    manifest_path=fixture.manifest,
                    status_command=("uv",),
                )

    def test_editable_source_cannot_overlap_read_only_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            shared = fixture.tenant / "shared"
            shared.mkdir()
            (fixture.sources / "shared").symlink_to(shared, target_is_directory=True)
            payload = fixture.payload()
            payload["sources"].append(
                fixture._source("shared", shared, "linked_path", False)
            )

            with self.assertRaisesRegex(ODOO_WORKSPACE.OdooWorkspaceError, "read-only source"):
                ODOO_WORKSPACE.validate_workspace_status(
                    payload,
                    manifest_path=fixture.manifest,
                    status_command=("uv",),
                )

    def test_workspace_inside_git_repository_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            subprocess.run(
                ["git", "init", "-q"],
                cwd=fixture.workspace,
                check=True,
            )

            with self.assertRaisesRegex(ODOO_WORKSPACE.OdooWorkspaceError, "outside a Git work tree"):
                ODOO_WORKSPACE.validate_workspace_status(
                    fixture.payload(),
                    manifest_path=fixture.manifest,
                    status_command=("uv",),
                )

    def test_workspace_below_ancestor_guidance_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            (fixture.root / "AGENTS.md").write_text(
                "# Untrusted ancestor guidance\n", encoding="utf-8"
            )

            with self.assertRaisesRegex(ODOO_WORKSPACE.OdooWorkspaceError, "ancestor guidance"):
                ODOO_WORKSPACE.validate_workspace_status(
                    fixture.payload(),
                    manifest_path=fixture.manifest,
                    status_command=("uv",),
                )


class OdooWorkspaceCommandTest(unittest.TestCase):
    def _launch(self, fixture: WorkspaceFixture):
        return ODOO_WORKSPACE.validate_workspace_status(
            fixture.payload(),
            manifest_path=fixture.manifest,
            status_command=("uv", "workspace", "status", "--check"),
        )

    def test_exec_uses_exact_workspace_roots_without_writable_workspace_cwd(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            codex_bin = fixture.root / "codex-lab"
            codex_bin.write_text("binary", encoding="utf-8")

            command = ODOO_WORKSPACE.build_codex_command(
                launch=self._launch(fixture),
                codex_bin=codex_bin,
                mode="exec",
                access="editable",
                prompt="inspect the workspace",
            )

            self.assertEqual(command[:4], (str(codex_bin), "exec", "--json", "--skip-git-repo-check"))
            self.assertIn("--sandbox", command)
            self.assertIn("workspace-write", command)
            self.assertNotIn("--add-dir", command)
            self.assertEqual(command.count("--workspace-root"), 2)
            self.assertIn(str(fixture.tenant.resolve()), command)
            self.assertIn(str(fixture.devkit.resolve()), command)
            workspace_roots = [
                command[index + 1]
                for index, value in enumerate(command)
                if value == "--workspace-root"
            ]
            self.assertNotIn(str(fixture.workspace.resolve()), workspace_roots)
            self.assertEqual(command[-2], "--")
            self.assertEqual(command[-1], "inspect the workspace")

    def test_option_like_prompt_is_separated_from_codex_flags(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            codex_bin = fixture.root / "codex-lab"
            codex_bin.write_text("binary", encoding="utf-8")

            command = ODOO_WORKSPACE.build_codex_command(
                launch=self._launch(fixture),
                codex_bin=codex_bin,
                mode="exec",
                access="editable",
                prompt="--dangerously-bypass-approvals-and-sandbox",
            )

            self.assertEqual(
                command[-2:],
                ("--", "--dangerously-bypass-approvals-and-sandbox"),
            )

    def test_interactive_read_only_path_is_explicit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            codex_bin = fixture.root / "codex-lab"
            codex_bin.write_text("binary", encoding="utf-8")

            command = ODOO_WORKSPACE.build_codex_command(
                launch=self._launch(fixture),
                codex_bin=codex_bin,
                mode="interactive",
                access="read-only",
                prompt=None,
            )

            self.assertEqual(command[0], str(codex_bin))
            self.assertNotIn("exec", command)
            self.assertIn("--sandbox", command)
            self.assertIn("read-only", command)
            self.assertNotIn("--add-dir", command)
            self.assertFalse(any("default_permissions" in value for value in command))

    def test_evidence_redacts_prompt_and_records_exact_permissions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = WorkspaceFixture(Path(temporary_directory))
            codex_bin = fixture.root / "codex-lab"
            codex_bin.write_text("binary", encoding="utf-8")
            launch = self._launch(fixture)
            prompt = "private task text"
            command = ODOO_WORKSPACE.build_codex_command(
                launch=launch,
                codex_bin=codex_bin,
                mode="exec",
                access="editable",
                prompt=prompt,
            )
            provenance = {"status": "current", "binary_sha256": "abc"}

            evidence = ODOO_WORKSPACE.build_evidence(
                launch=launch,
                codex_bin=codex_bin,
                provenance=provenance,
                command=command,
                mode="exec",
                access="editable",
                prompt=prompt,
                returncode=0,
            )

            serialized = json.dumps(evidence)
            self.assertNotIn(prompt, serialized)
            self.assertEqual(evidence["command"][-1], "<prompt>")
            self.assertFalse(evidence["permissions"]["workspace_root_writable"])
            self.assertEqual(
                evidence["permissions"]["writable_roots"],
                [str(fixture.tenant.resolve()), str(fixture.devkit.resolve())],
            )
            self.assertEqual(evidence["codex_binary"]["provenance"], provenance)


class OdooWorkspaceProcessTest(unittest.TestCase):
    def test_nonzero_status_uses_bounded_stale_reasons(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            devkit = root / "devkit"
            devkit.mkdir()
            manifest = root / "workspace.toml"
            manifest.write_text("schema_version = 1\n", encoding="utf-8")
            completed = subprocess.CompletedProcess(
                args=[],
                returncode=2,
                stdout=json.dumps(
                    {
                        "stale_reasons": [f"reason-{index}" for index in range(20)]
                    }
                ),
                stderr="ignored stderr",
            )

            with unittest.mock.patch.object(
                ODOO_WORKSPACE, "resolve_executable", return_value=Path("/usr/bin/uv")
            ), unittest.mock.patch.object(
                ODOO_WORKSPACE.subprocess, "run", return_value=completed
            ), self.assertRaisesRegex(
                ODOO_WORKSPACE.OdooWorkspaceError,
                r"reason-0.*reason-11, \.\.\.",
            ):
                ODOO_WORKSPACE.run_workspace_status(
                    uv_bin="uv",
                    devkit_path=devkit,
                    manifest_path=manifest,
                )

    def test_stale_binary_provenance_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            script = root / "scripts" / "local" / "codex_lab_provenance.py"
            script.parent.mkdir(parents=True)
            script.write_text("print('unused')\n", encoding="utf-8")
            binary = root / "codex-lab"
            binary.write_text("binary", encoding="utf-8")
            completed = subprocess.CompletedProcess(
                args=[],
                returncode=0,
                stdout=json.dumps({"status": "stale", "failures": ["commit mismatch"]}),
                stderr="",
            )

            with unittest.mock.patch.object(
                ODOO_WORKSPACE.subprocess, "run", return_value=completed
            ), self.assertRaisesRegex(
                ODOO_WORKSPACE.OdooWorkspaceError, "provenance is not current"
            ):
                ODOO_WORKSPACE.verify_codex_provenance(
                    source_repo=root,
                    codex_bin=binary,
                )


if __name__ == "__main__":
    unittest.main()
