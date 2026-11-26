"""Record contract/golden/*.json from the Python implementation (the reference).

This is the recorder, not the suite: it imports backend/app on purpose. The black-box suite only
reads the files it writes. Run after an intended behaviour change, then review the diff:
    backend/.venv/bin/python -B contract/record_golden.py
"""
from __future__ import annotations

import copy
import json
import os
import sys
import tempfile
from dataclasses import asdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
_tmp = tempfile.TemporaryDirectory(prefix="taketwo-golden-")
os.environ["TAKETWO_DATA"] = os.environ["VOICEFORGE_DATA"] = _tmp.name  # importing config creates folders; never the real library
sys.path.insert(0, str(HERE.parent / "backend"))

from app import pipeline, tools  # noqa: E402
from app.textproc import chunk_text, clean_text  # noqa: E402

FIXTURE = json.loads((HERE / "fixture.json").read_text())

CLEAN_CASES = [
    {"text": g["text"]} for g in FIXTURE["generations"]
] + [
    {"text": "Marika&39;s book &amp; &quot;notes&quot;&nbsp;here."},
    {"text": "As shown [2,3] and [4-6], it works [1]."},
    {"text": "As shown [2] here.", "strip_citations": False},
    {"text": "Wait — what? “Quoted” and ‘single’… done."},
    {"text": "**Bold** _under_ # heading `code` > quote | pipe ~ tilde"},
    {"text": "See https://example.com/page?x=1 for more."},
    {"text": "This line was hard\nwrapped in the middle of a sentence.\nA new sentence starts here."},
    {"text": "Short Title\nBody text follows here."},
    {"text": "A long-\nhouse and a well-\nKnown name."},
    {"text": "One.\n\n\n\nTwo."},
    {"text": "Use SQL and gif daily.", "pronunciations": [{"from": "SQL", "to": "sequel"}, {"from": "gif", "to": "jif"}, {"from": "", "to": "x"}]},
    {"text": "Line one\r\nLine two.\rLine three."},
    {"text": "   "},
]

LONG = ("This sentence is deliberately long, with several clauses; it keeps going: on and on, until it is far longer than "
        "the chunk limit allows, so the splitter must cut it at clause punctuation and then at spaces.")
CHUNK_CASES = [
    {"text": "Dr. Smith met Mr. Jones. They talked. Then J. R. Tolkien arrived!", "max_chars": 220},
    {"text": "First paragraph sentence one. Sentence two.\n\nSecond paragraph.", "max_chars": 220},
    {"text": "One sentence here. Another sentence there. A third one now. Ok.", "max_chars": 40},
    {"text": LONG, "max_chars": 60},
    {"text": "Short.\nAnother line.", "max_chars": 220},
    {"text": "这是一个很长的中文句子，没有空格，所以必须按字符切分。" * 6, "max_chars": 50},
    {"text": "x" * 130, "max_chars": 50},
    {"text": "", "max_chars": 220},
]

REVIEW_KEYS = ("part", "text", "issues", "reasons", "start", "end", "checked_by")


def main() -> None:
    clean = [{**c, "output": clean_text(c["text"], c.get("pronunciations"), c.get("strip_citations", True))} for c in CLEAN_CASES]
    for g in FIXTURE["generations"]:  # the fixture's stored cleanup must be what the reference makes of its script
        assert clean_text(g["text"]) == g["cleaned_text"], g["id"]
    chunks = [{**c, "chunks": [asdict(x) for x in chunk_text(c["text"], c["max_chars"])]} for c in CHUNK_CASES]
    narrations = {}
    for g in FIXTURE["generations"]:
        g = copy.deepcopy(g)
        out = {"outcome": pipeline.outcome(g)}
        out["review"] = [{k: r[k] for k in REVIEW_KEYS} for r in pipeline.review(g)] if out["outcome"] else []
        if g.get("timeline"):
            caps = tools.generation_captions(g)
            out.update(timings=tools.generation_timings(g), captions=caps,
                       srt=tools.to_srt(caps), vtt=tools.to_vtt(caps), txt=tools.to_txt(caps))
        narrations[g["id"]] = out
    for name, data in (("clean_text", clean), ("chunk_text", chunks), ("narrations", narrations)):
        (HERE / "golden" / f"{name}.json").write_text(json.dumps(data, indent=1, ensure_ascii=False) + "\n")
    print("Wrote contract/golden/clean_text.json, chunk_text.json, narrations.json")


if __name__ == "__main__":
    main()
