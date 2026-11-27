"""Read rust/out/whisper/parity.json (written by rust/scripts/whisper_parity.sh) and print the S13 table.

    python3 rust/scripts/whisper_report.py

Exit 0 only when: the model loaded; clips 1-3 with the default heads (top_two_layers) have
words_equal, monotonic and inside_clip true; and every number the table needs is present.
No timing tolerance is judged: the differences are printed, the lead sets the tolerance.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

PARITY = Path(__file__).resolve().parent.parent / "out/whisper/parity.json"
HEADS = ["top_two_layers", "large_v3_turbo"]
CLIPS = ["clip1", "clip2", "clip3", "clip4"]
NUMBERS = ["start_abs_diff_max", "start_abs_diff_median", "end_abs_diff_max", "end_abs_diff_median", "seconds", "python_seconds"]


def is_num(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def fmt(v) -> str:
    return f"{v:.3f}" if is_num(v) else str(v)


def main() -> int:
    r = json.loads(PARITY.read_text())
    problems = []
    if r.get("model_loaded") is not True:
        problems.append("model_loaded is not true")
    for k in ["peak_rss_bytes"]:
        if not is_num(r.get(k)):
            problems.append(f"{k} missing")
    if not isinstance(r.get("detected_language", {}).get("rust"), str):
        problems.append("detected language of the None case missing")

    print(f"Metal feature: {r.get('metal_feature')}, use_gpu: {r.get('use_gpu')}")
    print("| heads | clip | words equal | text equal | words R/P | monotonic | inside clip | start diff max / median (s) "
          "| end diff max / median (s) | segments R/P | avg_logprob R / P | no_speech_prob R / P | seconds R / P |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for h in HEADS:
        hd = r.get("heads", {}).get(h)
        if hd is None:
            problems.append(f"{h} missing")
            continue
        if not is_num(hd.get("load_seconds")):
            problems.append(f"{h} load_seconds missing")
        for c in CLIPS + (["clip1_auto"] if h == HEADS[0] else []):
            d = hd["clips"].get(c)
            if d is None:
                problems.append(f"{h} {c} missing")
                continue
            judged = h == HEADS[0] and c in CLIPS[:3]
            if judged:
                for k in ["words_equal", "monotonic", "inside_clip"]:
                    if d.get(k) is not True:
                        problems.append(f"{h} {c}: {k} is {d.get(k)}")
                for k in NUMBERS:
                    if not is_num(d.get(k)):
                        problems.append(f"{h} {c}: {k} missing")
                for k in ["avg_logprob", "no_speech_prob"]:
                    for side in ["rust", "python"]:
                        if not d[k][side] or not all(is_num(v) for v in d[k][side]):
                            problems.append(f"{h} {c}: {k} {side} missing")
            alp = " ".join(fmt(v) for v in d["avg_logprob"]["rust"]) + " / " + " ".join(fmt(v) for v in d["avg_logprob"]["python"])
            nsp = " ".join(fmt(v) for v in d["no_speech_prob"]["rust"]) + " / " + " ".join(fmt(v) for v in d["no_speech_prob"]["python"])
            print(f"| {h} | {c} | {d['words_equal']} | {d['text_equal']} | {d['word_count']['rust']}/{d['word_count']['python']} "
                  f"| {d['monotonic']} | {d['inside_clip']} | {fmt(d['start_abs_diff_max'])} / {fmt(d['start_abs_diff_median'])} "
                  f"| {fmt(d['end_abs_diff_max'])} / {fmt(d['end_abs_diff_median'])} | {d['segments']['rust']}/{d['segments']['python']} "
                  f"| {alp} | {nsp} | {fmt(d.get('seconds'))} / {fmt(d.get('python_seconds'))} |")
    print()
    for h in HEADS:
        print(f"load seconds {h}: {fmt(r['heads'].get(h, {}).get('load_seconds'))}")
    py = r.get("python", {})
    print(f"Python load seconds: {fmt(py.get('load_seconds'))}; Python alignment heads: {py.get('alignment_heads')}")
    peak = r.get("peak_rss_bytes")
    print(f"peak memory of the Rust test process: {peak / 2**30:.2f} GiB" if is_num(peak) else "peak memory: missing")
    if is_num(py.get("peak_rss_bytes")):
        print(f"peak memory of the Python process: {py['peak_rss_bytes'] / 2**30:.2f} GiB")
    print(f"detected language (language=None, clip1): {r.get('detected_language')}")
    print(f"cancel: {r.get('cancel')}")
    if problems:
        print("\nNOT PASSING:\n  " + "\n  ".join(problems))
        return 1
    print("\nwhisper_report: all required fields present; clips 1-3 (default heads) words_equal, monotonic, inside_clip")
    return 0


if __name__ == "__main__":
    sys.exit(main())
