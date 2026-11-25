"""The release check must propagate failures and isolate the saved library."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class CheckCommandTests(unittest.TestCase):
    def test_failed_frontend_blocks_release_and_backend_uses_temporary_data(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'scripts').mkdir()
            shutil.copyfile(ROOT / 'scripts/check.py', root / 'scripts/check.py')
            (root / 'frontend').mkdir()
            bindir = root / 'backend/.venv/bin'
            bindir.mkdir(parents=True)
            log = root / 'calls.jsonl'
            for name, body in [('python', 'exit_code = 0'), ('npm', 'exit_code = 9 if "build" in sys.argv else 0')]:
                path = bindir / name
                path.write_text(f'#!{Path(sys.executable).resolve()}\nimport json,os,sys\nfrom pathlib import Path\n{body}\nwith open(os.environ["CHECK_LOG"],"a") as f: f.write(json.dumps({{"args":sys.argv[1:],"data":os.environ.get("VOICEFORGE_DATA"),"electron":os.environ.get("ELECTRON_RUN_AS_NODE")}})+"\\n")\nsys.exit(exit_code)\n')
                path.chmod(0o755)
            env = {**os.environ, 'PATH': str(bindir) + os.pathsep + os.environ['PATH'], 'CHECK_LOG': str(log), 'VOICEFORGE_DATA': '/real/library', 'ELECTRON_RUN_AS_NODE': '1'}
            out = subprocess.run([sys.executable, str(root / 'scripts/check.py')], env=env, capture_output=True, text=True)
            self.assertEqual(out.returncode, 9, out.stdout + out.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            tests = next(c for c in calls if 'unittest' in c['args'])
            self.assertNotEqual(tests['data'], '/real/library')
            self.assertFalse(Path(tests['data']).exists())
            self.assertIsNone(tests['electron'])
            self.assertTrue(any('build' in c['args'] for c in calls))
