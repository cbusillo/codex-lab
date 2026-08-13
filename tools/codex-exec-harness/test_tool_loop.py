#!/usr/bin/env python3
"""Tests for exec-harness repeated failed-tool detection."""

import unittest

from tool_loop import detect_repeated_command_loop


class ToolLoopTest(unittest.TestCase):
    def test_detects_exact_threshold(self) -> None:
        self.assertEqual(
            "undefined-helper",
            detect_repeated_command_loop(
                ["undefined-helper", "undefined-helper", "undefined-helper"], 3
            ),
        )

    def test_ignores_non_consecutive_repeats(self) -> None:
        self.assertIsNone(
            detect_repeated_command_loop(
                ["undefined-helper", "echo ok", "undefined-helper"], 2
            )
        )

    def test_rejects_invalid_threshold(self) -> None:
        with self.assertRaisesRegex(ValueError, "at least 2"):
            detect_repeated_command_loop(["undefined-helper"], 1)


if __name__ == "__main__":
    unittest.main()
