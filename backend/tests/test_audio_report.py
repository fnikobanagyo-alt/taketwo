"""Export measurements must retain exceptions instead of claiming every pause passed."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import numpy as np
import soundfile as sf

ROOT = Path(__file__).resolve().parents[2]


class AudioReportTests(unittest.TestCase):
    def test_report_keeps_short_pause_and_hashes_the_measured_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sr = 16000
            tone = 0.3 * np.sin(2 * np.pi * 150 * np.arange(sr) / sr)
            path = root / 'final.wav'
            sf.write(path, np.concatenate([tone, np.zeros(3520), tone]), sr, subtype='FLOAT')
            manifest = {'cases': [{'id': 'short-stop', 'file': 'final.wav', 'stage': 'final', 'pauses': [{'label': 'brighter', 'window': [0.95, 1.27], 'target_seconds': 0.3}]}]}
            (root / 'manifest.json').write_text(json.dumps(manifest))
            result = subprocess.run([sys.executable, str(ROOT / 'scripts/audio_report.py'), '--manifest', str(root / 'manifest.json'), '--output', str(root / 'report.json'), '--skip-pitch'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((root / 'report.json').read_text())
            case = report['cases'][0]
            self.assertEqual(case['sha256'], hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertEqual(case['pauses'][0]['status'], 'short')
            self.assertAlmostEqual(case['pauses'][0]['seconds'], 0.22, delta=0.021)
            self.assertFalse(report['all_pause_targets_met'])
            self.assertIn('short', (root / 'report.md').read_text())
            self.assertEqual(case['pitch_status'], 'not_measured')
