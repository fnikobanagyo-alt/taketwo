"""A stop request during a blocked call must remain distinguishable from a stopped worker."""
import json
from pathlib import Path
import sys
import threading
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from app import worker


class WorkerDiagnosticsTests(unittest.TestCase):
    def test_blocked_phase_reports_pending_cancel_without_recording_script(self):
        entered, release, finished = threading.Event(), threading.Event(), threading.Event()

        def run(progress, cancel):
            with worker.phase('synthesis'):
                progress(0.1, 'PRIVATE SCRIPT MUST NOT BE EXPORTED')
                entered.set()
                release.wait(3)
            cancel()
            finished.set()

        job = worker.submit('generate', run, ref='diagnostic-fixture', label='PRIVATE VOICE')
        try:
            self.assertTrue(entered.wait(1))
            worker.cancel(job['id'])
            report = worker.diagnostics()
            current = next(j for j in report['jobs'] if j['id'] == job['id'])
            self.assertEqual(current['phase'], 'synthesis')
            self.assertEqual(current['status'], 'running')
            self.assertIsNotNone(current['cancel_requested_at'])
            self.assertIsNone(current['cancel_acknowledged_at'])
            self.assertGreaterEqual(current['phase_elapsed_seconds'], 0)
            self.assertIsNotNone(current['last_progress_at'])
            self.assertNotIn('PRIVATE', json.dumps(report))
            self.assertEqual(len(report['runtime']['source_sha256']), 64)
        finally:
            release.set()
        self.assertTrue(finished.wait(1))
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            current = next(j for j in worker.diagnostics()['jobs'] if j['id'] == job['id'])
            if current['finished'] is not None:
                break
            time.sleep(0.01)
        self.assertEqual(current['status'], 'cancelled')
        self.assertIsNotNone(current['cancel_acknowledged_at'])
        self.assertIsNotNone(current['finished'])

    def test_running_operation_is_kept_even_when_more_than_twenty_jobs_are_queued(self):
        entered, release = threading.Event(), threading.Event()
        def blocked(progress, cancel):
            with worker.phase('transcription'):
                entered.set()
                release.wait(3)
        job = worker.submit('generate', blocked, ref='oldest-running')
        try:
            self.assertTrue(entered.wait(1))
            for _ in range(25):
                worker.submit('test', lambda progress, cancel: None)
            report = worker.diagnostics()
            self.assertEqual(len(report['jobs']), 20)
            self.assertEqual(report['jobs'][0]['id'], job['id'])
            self.assertIsNotNone(report['jobs'][0]['last_progress_at'])
        finally:
            release.set()
