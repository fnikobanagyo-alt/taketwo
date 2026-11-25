#!/usr/bin/env python3
"""Measure saved WAVs without synthesis, ASR or library writes. JSON and Markdown share one result."""
import argparse
from datetime import datetime, timezone
import hashlib
import importlib.metadata
import json
from pathlib import Path

import numpy as np
import soundfile as sf

FRAME_SECONDS = 0.02
QUIET_BELOW_SPEECH_DB = 35.0
PITCH = {'name': 'librosa.pyin', 'sample_rate': 16000, 'fmin': 65, 'fmax': 450, 'frame_length': 1024, 'hop_length': 320}


def measure(case, base, skip_pitch):
    path = (base / case['file']).resolve()
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if case.get('sha256') and case['sha256'] != digest:
        raise ValueError(f"{case['id']}: fixture hash changed")
    audio, sr = sf.read(path, dtype='float32', always_2d=True)
    audio = audio.mean(axis=1)
    if not len(audio) or not np.isfinite(audio).all():
        raise ValueError(f"{case['id']}: empty or non-finite audio")
    hop = max(1, round(sr * FRAME_SECONDS))
    frames = [audio[i:i + hop] for i in range(0, len(audio), hop)]
    db = np.array([20 * np.log10(max(float(np.sqrt(np.mean(f.astype('float64') ** 2))), 1e-10)) for f in frames])
    threshold = float(np.percentile(db, 90)) - QUIET_BELOW_SPEECH_DB
    quiet = db <= threshold
    runs, start = [], None
    for i, is_quiet in enumerate(np.append(quiet, False)):
        if is_quiet and start is None:
            start = i
        elif not is_quiet and start is not None:
            runs.append({'start': start * hop / sr, 'end': min(i * hop / sr, len(audio) / sr)})
            start = None
    pauses = []
    for stop in case.get('pauses', []):
        low, high = stop['window']
        if not 0 <= low < high <= len(audio) / sr:
            raise ValueError(f"{case['id']}: invalid pause window {stop['label']}")
        candidates = [r for r in runs if r['start'] >= low and r['end'] <= high]
        gap = max(candidates, key=lambda r: r['end'] - r['start'], default=None)
        seconds = round(gap['end'] - gap['start'], 4) if gap else None
        target = stop.get('target_seconds')
        status = 'not_measured' if gap is None else ('measured' if target is None else ('met' if seconds >= target - FRAME_SECONDS else 'short'))
        pauses.append({**stop, 'seconds': seconds, 'status': status, 'measured_window': gap})
    pitch = None
    voiced_fraction = None
    if not skip_pitch:
        import librosa
        y = librosa.resample(audio, orig_sr=sr, target_sr=PITCH['sample_rate'])
        f0, voiced, _ = librosa.pyin(y, sr=PITCH['sample_rate'], fmin=PITCH['fmin'], fmax=PITCH['fmax'], frame_length=PITCH['frame_length'], hop_length=PITCH['hop_length'])
        valid = voiced & np.isfinite(f0)
        voiced_fraction = float(valid.mean())
        pitch = float(np.median(f0[valid])) if valid.any() else None
    return {'id': case['id'], 'file': str(path), 'stage': case['stage'], 'sha256': digest,
            'duration_seconds': len(audio) / sr, 'sample_rate': sr, 'settings': case.get('settings', {}),
            'pitch_hz': pitch, 'pitch_status': 'measured' if pitch is not None else 'not_measured',
            'voiced_fraction': voiced_fraction, 'quiet_threshold_db': threshold, 'quiet_runs': runs,
            'pauses': pauses, 'boundary_coverage': case.get('boundary_coverage', 'supplied windows only'), 'listening_acceptance': case.get('listening_acceptance', 'not_verified')}


def markdown(report):
    lines = ['# Saved-audio measurements', '', 'Automated measurements only. Pause windows are supplied annotations, not verified word alignment. Listening acceptance is separate.', '',
             f"Pitch estimator: {report['pitch_estimator']}. Quiet threshold: 35 dB below the 90th-percentile frame level; 20 ms frames.", '',
             '| Case | Stage | Median pitch (Hz) | Listening |', '|---|---|---:|---|']
    for case in report['cases']:
        pitch = f"{case['pitch_hz']:.1f}" if case['pitch_hz'] is not None else 'not measured'
        lines.append(f"| {case['id']} | {case['stage']} | {pitch} | {case['listening_acceptance']} |")
    lines += ['', '| Case / stop | Measured seconds | Target seconds | Result |', '|---|---:|---:|---|']
    for case in report['cases']:
        for pause in case['pauses']:
            lines.append(f"| {case['id']} / {pause['label']} | {pause['seconds']} | {pause.get('target_seconds')} | {pause['status']} |")
    lines += ['', f"All supplied pause targets met: {report['all_pause_targets_met']}. This is not a naturalness judgement.", '', '## Audio', '']
    lines += [f"- [{case['id']}](<{case['file']}>)" for case in report['cases']]
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--skip-pitch', action='store_true', help='explicitly mark pitch not measured')
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text())
    inputs = {args.manifest.resolve()} | {(args.manifest.parent / c['file']).resolve() for c in manifest['cases']}
    if args.output.resolve() in inputs or args.output.with_suffix('.md').resolve() in inputs:
        parser.error('Report would overwrite an input')
    cases = [measure(c, args.manifest.parent, args.skip_pitch) for c in manifest['cases']]
    targets = [p for c in cases for p in c['pauses'] if p.get('target_seconds') is not None]
    versions = {name: importlib.metadata.version(name) for name in ('numpy', 'soundfile', 'librosa')}
    report = {'created_at': datetime.now(timezone.utc).isoformat(), 'manifest_sha256': hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
              'reporter_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), 'versions': versions,
              'pitch_estimator': {**PITCH, 'enabled': not args.skip_pitch},
              'pause_estimator': {'frame_seconds': FRAME_SECONDS, 'quiet_below_speech_db': QUIET_BELOW_SPEECH_DB, 'target_tolerance_seconds': FRAME_SECONDS},
              'annotation_status': manifest.get('annotation_status', 'not supplied'), 'cases': cases, 'all_pause_targets_met': all(p['status'] == 'met' for p in targets) if targets else None}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + '\n')
    args.output.with_suffix('.md').write_text(markdown(report))
    print(args.output)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
