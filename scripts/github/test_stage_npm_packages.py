#!/usr/bin/env python3
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "stage_npm_packages.py"


def load_module():
    spec = importlib.util.spec_from_file_location("stage_npm_packages", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load module from {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


stage_npm_packages = load_module()


class StageNpmPackagesTest(unittest.TestCase):
    def test_expand_packages_expands_root_package_by_default(self) -> None:
        self.assertEqual(
            stage_npm_packages.PACKAGE_EXPANSIONS["codex"],
            stage_npm_packages.expand_packages(["codex"]),
        )

    def test_expand_packages_can_preserve_requested_packages(self) -> None:
        self.assertEqual(
            ["codex", "codex-sdk"],
            stage_npm_packages.expand_packages(
                ["codex", "codex-sdk", "codex"],
                expand=False,
            ),
        )

    def test_parse_args_accepts_no_expand_packages(self) -> None:
        with mock.patch.object(
            sys,
            "argv",
            [
                str(SCRIPT),
                "--release-version",
                "0.0.0",
                "--package",
                "codex",
                "--no-expand-packages",
            ],
        ):
            args = stage_npm_packages.parse_args()

        self.assertTrue(args.no_expand_packages)

    def test_main_expands_and_stages_platform_packages_hermetically(self) -> None:
        build_module = stage_npm_packages._BUILD_MODULE
        staged: dict[str, Path] = {}
        install_calls: list[set[str]] = []

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            runner_temp = root / "runner"
            artifacts_dir = root / "artifacts"
            output_dir = root / "output"
            runner_temp.mkdir()

            def fake_install_native_components(
                workflow_url: str,
                components: set[str],
                vendor_root: Path,
                artifacts_root: Path,
            ) -> None:
                self.assertEqual("https://example.test/actions/runs/1", workflow_url)
                self.assertEqual(artifacts_dir.resolve(), artifacts_root)
                install_calls.append(components)
                vendor = vendor_root / "vendor"
                for target in stage_npm_packages.BINARY_TARGETS:
                    target_dir = vendor / target
                    target_dir.mkdir(parents=True)
                    binary = target_dir / (
                        "codex.exe" if "windows" in target else "codex"
                    )
                    binary.write_text("binary\n", encoding="utf-8")

            def fake_run_command(command: list[str]) -> None:
                package = command[command.index("--package") + 1]
                self.assertEqual(0, build_module.main(command[1:]))
                staged[package] = Path(command[command.index("--staging-dir") + 1])

            def fake_run_npm_pack(staging_dir: Path, output_path: Path) -> Path:
                output_path.parent.mkdir(parents=True, exist_ok=True)
                output_path.write_text(staging_dir.name, encoding="utf-8")
                return output_path

            argv = [
                str(SCRIPT),
                "--release-version",
                "0.0.0",
                "--package",
                "codex",
                "--workflow-url",
                "https://example.test/actions/runs/1",
                "--artifacts-dir",
                str(artifacts_dir),
                "--output-dir",
                str(output_dir),
                "--keep-staging-dirs",
            ]
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.dict("os.environ", {"RUNNER_TEMP": str(runner_temp)}),
                mock.patch.object(
                    stage_npm_packages,
                    "install_native_components",
                    side_effect=fake_install_native_components,
                ),
                mock.patch.object(
                    stage_npm_packages,
                    "run_command",
                    side_effect=fake_run_command,
                ),
                mock.patch.object(
                    build_module,
                    "run_npm_pack",
                    side_effect=fake_run_npm_pack,
                ),
            ):
                self.assertEqual(0, stage_npm_packages.main())

            expected_packages = stage_npm_packages.PACKAGE_EXPANSIONS["codex"]
            self.assertEqual(set(expected_packages), set(staged))
            self.assertEqual(
                [{stage_npm_packages.CODEX_PACKAGE_COMPONENT}], install_calls
            )
            root_package = json.loads(
                (staged["codex"] / "package.json").read_text(encoding="utf-8")
            )
            self.assertEqual(
                len(stage_npm_packages.CODEX_PLATFORM_PACKAGES),
                len(root_package["optionalDependencies"]),
            )
            for package, config in stage_npm_packages.CODEX_PLATFORM_PACKAGES.items():
                target = config["target_triple"]
                self.assertTrue((staged[package] / "vendor" / target).is_dir())
            self.assertEqual(
                {
                    stage_npm_packages.tarball_name_for_package(package, "0.0.0")
                    for package in expected_packages
                },
                {path.name for path in output_dir.iterdir()},
            )


if __name__ == "__main__":
    unittest.main()
