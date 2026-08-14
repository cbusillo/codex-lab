import tempfile
import unittest
from pathlib import Path

import upstream_convergence_bazel_data as bazel_data


class BazelDataFixture(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        (self.root / "BUILD.bazel").write_text("exports_files([])\n", encoding="utf-8")

    def write(self, relative: str, contents: str = "") -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")
        return path

    def test_cross_package_edge_passes_with_compile_data_and_export(self) -> None:
        self.write(
            "codex-rs/consumer/BUILD.bazel",
            'codex_rust_crate(compile_data = ["//codex-rs/producer:asset.txt"])\n',
        )
        self.write(
            "codex-rs/consumer/src/lib.rs", 'const ASSET: &str = include_str!("../../producer/asset.txt");\n'
        )
        self.write("codex-rs/producer/BUILD.bazel", 'exports_files(["asset.txt"])\n')
        self.write("codex-rs/producer/asset.txt", "asset\n")

        report = bazel_data.verify(self.root)

        self.assertEqual([], report["errors"])
        self.assertTrue(report["passed"])

    def test_cross_package_edge_requires_consumer_label(self) -> None:
        self.write("codex-rs/consumer/BUILD.bazel", "codex_rust_crate()\n")
        self.write(
            "codex-rs/consumer/src/lib.rs", 'const ASSET: &str = include_str!("../../producer/asset.txt");\n'
        )
        self.write("codex-rs/producer/BUILD.bazel", 'exports_files(["asset.txt"])\n')
        self.write("codex-rs/producer/asset.txt", "asset\n")

        report = bazel_data.verify(self.root)

        self.assertTrue(
            any("missing compile_data label" in error for error in report["errors"])
        )

    def test_cross_package_edge_requires_producer_export(self) -> None:
        self.write(
            "codex-rs/consumer/BUILD.bazel",
            'codex_rust_crate(compile_data = ["//codex-rs/producer:asset.txt"])\n',
        )
        self.write(
            "codex-rs/consumer/src/lib.rs", 'const ASSET: &str = include_str!("../../producer/asset.txt");\n'
        )
        self.write("codex-rs/producer/BUILD.bazel", "exports_files([])\n")
        self.write("codex-rs/producer/asset.txt", "asset\n")

        report = bazel_data.verify(self.root)

        self.assertTrue(any("does not export" in error for error in report["errors"]))

    def test_migration_directory_requires_compile_data_glob(self) -> None:
        self.write("codex-rs/state/BUILD.bazel", "codex_rust_crate()\n")
        self.write(
            "codex-rs/state/src/migrations.rs",
            'static MIGRATOR: Migrator = sqlx::migrate!("./migrations");\n',
        )
        self.write("codex-rs/state/migrations/001.sql", "select 1;\n")

        report = bazel_data.verify(self.root)

        self.assertTrue(
            any("does not cover migration directory" in error for error in report["errors"])
        )

    def test_migration_directory_accepts_compile_data_glob(self) -> None:
        self.write(
            "codex-rs/state/BUILD.bazel",
            'codex_rust_crate(compile_data = glob(["migrations/**"]))\n',
        )
        self.write(
            "codex-rs/state/src/migrations.rs",
            'static MIGRATOR: Migrator = sqlx::migrate!("./migrations");\n',
        )
        self.write("codex-rs/state/migrations/001.sql", "select 1;\n")

        report = bazel_data.verify(self.root)

        self.assertEqual([], report["errors"])

    def test_non_literal_include_fails_closed(self) -> None:
        self.write("codex-rs/sample/BUILD.bazel", "codex_rust_crate()\n")
        self.write(
            "codex-rs/sample/src/lib.rs",
            'const ASSET: &str = include_str!(concat!("../", NAME));\n',
        )

        report = bazel_data.verify(self.root)

        self.assertTrue(any("unsupported non-literal" in error for error in report["errors"]))

    def test_allowlisted_dynamic_include_count_is_bounded(self) -> None:
        self.write("codex-rs/tui/BUILD.bazel", "codex_rust_crate()\n")
        self.write(
            "codex-rs/tui/src/frames.rs",
            'const ASSET: &str = include_str!(concat!("../", NAME));\n',
        )

        report = bazel_data.verify(self.root)

        self.assertTrue(any("expected 36, found 1" in error for error in report["errors"]))

    def test_missing_local_include_target_is_reported(self) -> None:
        self.write("codex-rs/sample/BUILD.bazel", "codex_rust_crate()\n")
        self.write(
            "codex-rs/sample/src/lib.rs",
            'const ASSET: &str = include_str!("missing.txt");\n',
        )

        report = bazel_data.verify(self.root)

        self.assertTrue(any("compile-time file is missing" in error for error in report["errors"]))


class CheckedInBazelDataTest(unittest.TestCase):
    def test_checked_in_compile_time_edges_are_complete(self) -> None:
        report = bazel_data.verify(bazel_data.REPO_ROOT)

        self.assertEqual([], report["errors"])
        self.assertTrue(report["passed"])


if __name__ == "__main__":
    unittest.main()
