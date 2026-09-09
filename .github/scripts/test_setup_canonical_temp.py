import subprocess
import unittest
from pathlib import Path

ACTION_DIR = Path(__file__).resolve().parents[1] / "actions" / "setup-canonical-temp"


class SetupCanonicalTempNodeTests(unittest.TestCase):
    def test_node_action_suite(self) -> None:
        result = subprocess.run(
            ["node", "--test", "test.js", "../prepare-bazel-cleanup/test.js"],
            cwd=ACTION_DIR,
            check=False,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            self.fail(
                "node canonical temp tests failed\n"
                f"stdout:\n{result.stdout}\n"
                f"stderr:\n{result.stderr}"
            )


if __name__ == "__main__":
    unittest.main()
