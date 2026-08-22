import json
import os
import subprocess
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from unittest.mock import patch, MagicMock

import upstream_convergence_report as ucr

class TestUpstreamConvergenceReport(unittest.TestCase):
    @patch('upstream_convergence_report.run_git')
    def test_build_report_no_alert(self, mock_git):
        # mock rev-list and show
        def mock_git_call(*args):
            if args[0] == 'rev-list':
                return '10'
            if args[0] == 'show':
                return datetime.now(timezone.utc).isoformat()
            return ''
        mock_git.side_effect = mock_git_call
        
        report_data = {
            "refs": {
                "base": "0000base",
                "upstream": "0000up",
                "local": "0000loc"
            },
            "inventory": {
                "summary": {
                    "conflicts": 2,
                    "residualLocalInfluence": 1
                },
                "laneCounts": {
                    "LANE-1": 1
                }
            }
        }
        
        now = datetime.now(timezone.utc)
        text, alert = ucr.build_report(report_data, now)
        
        self.assertFalse(alert)
        self.assertIn("- **Lag**: 10 commits", text)
        self.assertIn("- **Age**: 0.0 hours", text)
        self.assertIn("- **Conflicts**: 2", text)
        self.assertIn("- **Residuals**: 1", text)
        self.assertIn("- **Lanes**: 1", text)
        self.assertNotIn("⚠️ **ALERT**", text)

    @patch('upstream_convergence_report.run_git')
    def test_build_report_lag_alert(self, mock_git):
        def mock_git_call(*args):
            if args[0] == 'rev-list':
                return '100' # > 75
            if args[0] == 'show':
                return datetime.now(timezone.utc).isoformat()
            return ''
        mock_git.side_effect = mock_git_call
        
        report_data = {
            "refs": {
                "base": "0000base",
                "upstream": "0000up",
                "local": "0000loc"
            }
        }
        
        now = datetime.now(timezone.utc)
        text, alert = ucr.build_report(report_data, now)
        self.assertTrue(alert)
        self.assertIn("exceeds threshold (75 commits)", text)

    @patch('upstream_convergence_report.run_git')
    def test_build_report_stale_alert(self, mock_git):
        def mock_git_call(*args):
            if args[0] == 'rev-list':
                return '10'
            if args[0] == 'show':
                return (datetime.now(timezone.utc) - timedelta(hours=80)).isoformat()
            return ''
        mock_git.side_effect = mock_git_call
        
        report_data = {
            "refs": {
                "base": "0000base",
                "upstream": "0000up",
                "local": "0000loc"
            }
        }
        
        now = datetime.now(timezone.utc)
        text, alert = ucr.build_report(report_data, now)
        self.assertTrue(alert)
        self.assertIn("exceeds threshold (72 hours)", text)

if __name__ == '__main__':
    unittest.main()
