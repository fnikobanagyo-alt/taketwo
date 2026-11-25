#!/usr/bin/env python3
"""Run VoiceForge's release checks against a disposable library."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--preflight', action='store_true', help='check prerequisites without running the suite')
    parser.add_argument('--ui', action='store_true', help='also run Electron playback and recovery checks')
    args = parser.parse_args()
    python = ROOT / 'backend/.venv/bin/python'
    npm = shutil.which('npm')
    if not python.is_file() or not npm:
        print('Missing backend/.venv/bin/python or npm. Run ./start.sh --setup first.', flush=True)
        return 1
    env = os.environ.copy()
    env.pop('ELECTRON_RUN_AS_NODE', None)
    with tempfile.TemporaryDirectory(prefix='voiceforge-check-') as data:
        env['VOICEFORGE_DATA'] = data
        commands = [([npm, 'exec', '--no', '--', 'tsc', '--version'], ROOT / 'frontend')]
        if not args.preflight:
            commands += [([str(python), '-B', '-m', 'unittest', 'discover', '-s', 'backend/tests'], ROOT),
                         ([npm, 'run', 'build'], ROOT / 'frontend')]
            if args.ui:
                commands += [([npm, 'run', name], ROOT / 'frontend') for name in ('test:playback', 'test:recovery')]
        for command, cwd in commands:
            print('Checking: ' + ' '.join(command), flush=True)
            result = subprocess.run(command, cwd=cwd, env=env)
            if result.returncode:
                return result.returncode
    print('Checks passed.', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
