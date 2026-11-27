"""Fail if anything private from private/scripts.json shows up in a tracked file that quotes the A/B harness.

    python3 rust/ab/check_private.py

Checked: every title, every 40-character run of every script (raw and cleaned) and every voice name,
against rust/PROGRESS.md, rust/ab/README.md and rust/ab/ab.py. Exit 1 on any hit (only counts are printed).
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUN = 40

plan_file = HERE / "private" / "scripts.json"
if not plan_file.exists():
    sys.exit("No private/scripts.json: run  python3 rust/ab/ab.py plan  first.")
plan = json.loads(plan_file.read_text())
docs = {p.name: p.read_text() for p in (HERE.parent / "PROGRESS.md", HERE / "README.md", HERE / "ab.py")}
names = [s.get("title") for s in plan["scripts"]] + [v.get("name") for v in plan["voices"]]
texts = [t for s in plan["scripts"] for t in (s.get("text"), s.get("cleaned_text")) if t]
hits = 0
for doc_name, doc in docs.items():
    runs = {doc[i:i + RUN] for i in range(len(doc) - RUN + 1)}
    n = sum(1 for x in names if x and x in doc)
    n += sum(1 for t in texts for i in range(len(t) - RUN + 1) if t[i:i + RUN] in runs)
    if n:
        print(f"{doc_name}: {n} private matches")
    hits += n
print(f"checked {len(names)} titles and voice names, {len(texts)} script texts against {len(docs)} files: {hits} matches")
sys.exit(1 if hits else 0)
