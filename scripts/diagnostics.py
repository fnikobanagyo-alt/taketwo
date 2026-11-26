#!/usr/bin/env python3
"""Print TakeTwo's bounded diagnostic snapshot without changing runtime state."""
import json
from pathlib import Path
import urllib.error
import urllib.parse
import urllib.request


def main():
    try:
        discovery = Path.home() / 'Library/Application Support/TakeTwo/engine.json'
        url = json.loads(discovery.read_text())['url']
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme != 'http' or parsed.hostname not in ('127.0.0.1', 'localhost', '::1'):
            raise ValueError('Engine discovery must identify a local HTTP backend')
        with urllib.request.urlopen(url.rstrip('/') + '/api/diagnostics', timeout=5) as response:
            report = json.load(response)
        print(json.dumps(report, indent=2))
        return 0
    except (OSError, ValueError, KeyError, urllib.error.URLError) as exc:
        print(json.dumps({'error': str(exc), 'hint': 'Check engine discovery. Older backends need an authorized idle restart for the diagnostics route.'}))
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
