"""Record Python parity vectors for the Rust `text` (S03), `library` (S04), `captions` and `review` (S05) modules.

    backend/.venv/bin/python -B rust/golden/record.py

Writes rust/golden/text.json: synthetic inputs and what the Python app returns for them
(textproc, ml.normalize_for_wer, ml.wer, difflib ratio, pipeline word coverage).
Writes rust/golden/library.sqlite3 (a database written by db.py, synthetic documents only) and
rust/golden/library.json (a JSON copy of its documents, settings vectors, and what
contract/seed_python.py seed() writes into a temporary folder, recorded in a child process).
db.py's clock is pinned, so both files come out byte-identical on every run.
Writes rust/golden/review.json (step S05): tools.py captions, timings, subtitles and download header,
and pipeline.py review rules, "Sounds fine" and take bookkeeping, on synthetic documents
(pipeline's clock pinned; take files live in the temporary generations folder, paths recorded relative).
Safety: the data folder is a fresh temporary folder (checked before any other app import),
Hugging Face is offline, and the Whisper normalizer comes from tokenizer files only.
No model is loaded or downloaded: ml.stt_model is replaced by a function that raises.
"""
from __future__ import annotations

import copy
import difflib
import json
import os
import random
import shutil
import sqlite3
import struct
import subprocess
import sys
import tempfile
import types
import wave
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
BACKEND = WT / "backend"
OUT = HERE / "text.json"
LIB_OUT = HERE / "library.json"
LIB_DB = HERE / "library.sqlite3"
SPELLING = WT / "rust/crates/taketwo-core/data/whisper_normalizer.json"


NORMALIZE = [
    "", "   ", "\n", "Hello, World!", "  leading and trailing  ",
    # number words
    "one", "ones", "one two three four", "twenty one", "twenty-one", "nineteen ninety nine",
    "two thousand and twenty", "one hundred and five", "a hundred people", "three million",
    "one oh one", "zero zero seven", "0 oh", "oh my god", "double five", "triple zero",
    "double trouble", "two point five", "point five", "point", "point hundred", "five point",
    "three and a half", "and a half", "half past one", "one and two",
    "ten thousand two hundred and fifty", "five hundred thousand", "1.5 million", ".5 million",
    "1.2.3 million", "one decillion", "one decillion decillion",
    "123456789012345678901234567890123456789012345 decillion",
    "99999999999999999999 hundred", "seven hundreds", "two thousands", "millions of years",
    # ordinals and plurals
    "first second third", "the fourth fifth and sixth", "eighth ninth", "nineth", "twelfth night",
    "twentieth century", "twenty first century", "one hundredth", "millionth", "the 21st", "1st",
    "the 1960s", "sixes and sevens", "the twenties", "the nineteen twenties", "twenty twenties",
    "thirty fifth", "two hundred thirty fifth",
    # currency and percent
    "$20 million", "$20", "20 dollars", "five dollars", "dollars", "$2 and 7 cents",
    "$2 and ¢7", "£0.50", "€3.05", "fifty cents", "5 pounds", "ten euros", "a pound of flour",
    "five percent", "ten per cent", "per cent", "per", "50%", "100 % sure", "the % sign",
    "minus three", "minus", "plus five", "negative 2.5", "positive thinking", "+5 points",
    "-3 degrees", "1,000,000 people", "1,2,3", "3.0", "007", "3.14159", "192.168.0.1",
    "2 1/2", "4.5.6", "$5.$5", "1s", "the 1 and only", "1 s", "11 s", "number 1",
    # contractions and titles
    "I won't go", "you can't", "let's go", "it ain't so", "y'all", "I wanna", "gotta go",
    "gonna", "i'ma", "imma", "woulda coulda shoulda", "yes ma'am", "it's been", "she'd been",
    "he's gone", "they'd gone", "we'd done", "she's got", "don't", "they're", "it's",
    "I'd", "we'll", "isn't", "I've", "I'm", "rock 'n' roll", "the dog 's bone",
    "Mr Smith", "Mrs. Jones", "Dr. Who", "St. John", "Prof Plum", "Capt. Hook", "Gov",
    "Ald", "Gen. Lee", "Sen Rep Pres Rev Hon", "Asst Assoc Lt Col", "Jr. and Sr.", "John Esq",
    "mister", "street", "dr.", "drs", "stop",
    # British spellings
    "colour", "The organisation's favourite colours", "realise", "analyse", "centre theatre",
    "travelled", "grey", "aeroplane", "programme", "defence", "licence",
    # diacritics and symbols
    "café", "café", "naïve", "naïve", "œuvre", "Straße", "Ærø", "ðþ", "Łódź",
    "Ångström", "piñata", "Ελληνικά", "Русский", "日本語のテキスト", "한국어", "x²", "½",
    "１２３", "٣ dollars", "Hello 😀 world", "a\u001cb", "tab\there", "line\nbreak", "a b",
    "smart “quotes” and ‘single’", "em—dash", "en–dash", "hide-working", "hideworking",
    "e.g. this", "U.S.A.", "end.", "a..b", "wait...", "#hashtag", "@user", "a & b", "100°",
    "©2024", "™", "a/b", "C++", "under_score", "x*y", "Ünïcödé",
    # brackets and fillers
    "[inaudible] hello", "(laughs) yes", "<unk> word", "[a] b [c]", "(unclosed", "a [b",
    "um uh hmm mm mhm mmm", "umbrella", "hmmm", "summary", "mmmm",
]

WER = [
    ("", ""), ("", "hello"), ("hello", ""), ("   ", "  "), ("(only brackets)", ""),
    ("hello world", "hello world"), ("hello world", "hello there world"),
    ("the quick brown fox", "the quick brown"), ("the quick brown fox", "quick brown fox jumps"),
    ("hide-working", "hideworking"), ("hide working", "hideworking"),
    ("She said (quietly) hello.", "She said quietly hello."),
    ("Use the [red] button", "Use the red button"), ("{curly} braces", "curly braces"),
    ("I have 20 apples", "I have twenty apples"), ("It costs $5", "It costs five dollars"),
    ("Mr. Smith arrived.", "Mister Smith arrived"), ("The colour is grey.", "the color is gray"),
    ("won't", "will not"), ("Scadutree", "Skadu tree"),
    ("Beneath the Scadutree.", "Beneath the Skadu tree."),
    ("When the long day was finally over they asked me to come home.",
     "the long day was finally over they asked me to come home."),
    ("two twin demigods: Miquella and Malenia.", "two twin demigods, Mykela and Melania."),
    ("a b c d e", "e d c b a"), ("one", "two three four five"), ("abc", "xyz"),
    ("café au lait", "cafe au lait"), ("hello", "hello hello hello"),
    ("The year 1999 was long.", "the year nineteen ninety nine was long"),
    ("um so yes", "so yes"), ("(laughs)", "ha"), ("ab", "ba"),
    ("the cat sat on the mat", "a cat sat on a mat"), ("Hello, world!", "hello world"),
    ("日本語", "日本語"), ("x", "y"),
]

RATIO = [
    ("", ""), ("a", ""), ("", "a"), ("abc", "abc"), ("abc", "xyz"), ("home", "come"),
    ("scadutree", "skadu"), ("scadutree", "skadutree"), ("scadutree", "skadutreestood"),
    ("miquella", "mykela"), ("malenia", "melania"), ("abcd", "bcda"), ("abxcd", "abcd"),
    ("private", "privateer"), ("aaaa", "aa"), ("ab", "ba"), ("hello", "hallo"),
    ("kitten", "sitting"), ("tolkien", "tolkein"), ("dr", "doctor"), ("1999", "1990"),
    ("naïve", "naive"), ("日本語", "日本"), ("qwertyuiop", "poiuytrewq"),
    ("thequickbrownfox", "quickbrownthefox"), ("abcabcabc", "cabcab"), ("a" * 10, "a" * 3 + "b" + "a" * 6),
    ("x" + "ab" * 120, "ab" * 120 + "y"),
    ("the" * 70 + "end", "e" * 50 + "the" * 60),
    ("abcdefghij" * 25, "jihgfedcba" * 21),
    ("mississippi", "missisippi"), ("returning", "returninghome"),
]

COVERAGE = [
    ("When the long day was finally over they asked me to come home.",
     "the long day was finally over they asked me to come home."),
    ("When the long day was finally over they asked me to come home.",
     "When the long day was finally over they asked me to come home."),
    ("When the long day was finally over they asked me to come home.",
     "When the long day was finally over they asked me to come."),
    ("Scadutree stood tall.", "Skadu tree stood tall."),
    ("Beneath the Scadutree.", "Beneath the Skadu tree."),
    ("nestled in the shadow of the Scadutree.", "nestled in the shadow of the Skadu tree."),
    ("two twin demigods: Miquella and Malenia.", "two twin demigods, Mykela and Melania."),
    ("We walked before returning home.", "We walked before returning."),
    ("", ""), ("", "something heard"), ("Hello there.", ""), ("Hello.", "Hello."),
    ("Hello.", "Goodbye."), ("I have 20 apples.", "I have twenty apples."),
    ("It costs $5.", "It costs five dollars."), ("Mr. Smith arrived.", "Mister Smith arrived."),
    ("The colour is grey.", "The color is gray."), ("She won't stay.", "She will not stay."),
    ("one two three four", "one three four"), ("one two three four", "one two three"),
    ("one two three four", "two three four"), ("a b c", "c b a"),
    ("The quick brown fox jumps.", "The quick brown fox jumps over the lazy dog."),
    ("The quick brown fox jumps over the lazy dog.", "quick fox lazy"),
    ("Hideworking is hard.", "Hide working is hard."), ("Tolkien wrote it.", "Tolkein wrote it."),
    ("Ender's Game", "Enders Game"), ("come home", "come come"), ("home", "come home"),
    ("Malenia, Blade of Miquella.", "Melania blade of Mikayla."),
    ("Rennala, Queen of the Full Moon.", "Renala queen of the full moon"),
    ("They asked me to come home", "They asked me to come home home"),
    ("(aside) the end.", "the end."), ("Café au lait.", "Cafe au lait."),
    ("Double five, then point two.", "55 then .2"),
    # right on the 0.6 limit (ratio 0.6 exactly, and just under)
    ("abcde went", "abcxy went"), ("went abcde", "went abcxy"), ("abcdefg went", "abcdxyz went"),
    ("abcdef", "abc xyz"), ("abcdefghij", "abcdefxyzw"), ("abcdefghij", "abcde fxyzw"),
]

CLEAN = [
    {"text": "我们今天讨论河流。“城市”在河边！"},
    {"text": "‘Single’ and “double” curly quotes… with an ellipsis"},
    {"text": "No break spaces here."},
    {"text": "Marika&39;s book &amp; Radagon&39;s &quot;notes&quot;"},
    {"text": "Bare &9; &31; &32; &8191; &8192; &99999; numerics"},
    {"text": "Hex &#x27;quote&#X27; and &#39; decimal, &#128; cp1252, &#0; zero, &#13; cr"},
    {"text": "Bad &#xD800; surrogate, &#99999999999; huge, &#1; control, &#65535; noncharacter"},
    {"text": "Legacy &copy 2024 &amp &lt;tag&gt; &notit; &notin; &AMP; &bogus; & alone &#; &#x;"},
    {"text": "Long &abcdefghijklmnopqrstuvwxyzabcdefghij; name and &;"},
    {"text": "Rivers — towns – and more—less"},
    {"text": "Cited fact [1]. Another [2,3] and [4-6] and [7–9] and [10, 11]."},
    {"text": "Keep [1] these [2,3]", "strip_citations": False},
    {"text": "**Bold** _under_ #head `code` > quote | pipe ~ tilde"},
    {"text": "See https://example.com/page?x=1 and http://a.b now."},
    {"text": "The SQL server and sql tools", "pronunciations": [{"from": "SQL", "to": "sequel"}]},
    {"text": "C++ and C# are languages", "pronunciations": [{"from": "C++", "to": "C plus plus"}]},
    {"text": "It costs five", "pronunciations": [{"from": "five", "to": "$5 or $1"}]},
    {"text": "Café culture at the cafe", "pronunciations": [{"from": "café", "to": "ka-fay"}]},
    {"text": "Ignore blanks", "pronunciations": [{"from": " ", "to": "x"}, {"from": "a", "to": ""}, {"to": "y"}]},
    {"text": "Wordy words wordy", "pronunciations": [{"from": "word", "to": "WORD"}, {"from": "wordy", "to": "verbose"}]},
    {"text": "A line that was hard\nwrapped in the middle of\na sentence and goes on."},
    {"text": "Heading\nBody text starts here.\nIt continues."},
    {"text": "A long-\nhouse and a well-\nKnown name, then a comma,\nNext line."},
    {"text": "Para one.\n\n\n\nPara two.\r\nWindows line.\rOld Mac line."},
    {"text": "  spaced   out\t\ttabs  \n  and lines  "},
    {"text": ", leading comma\n, another\nand ,, doubled , , commas"},
    {"text": "Decomposed café and naïve text."},
    {"text": "한국어 문장입니다. 日本語の文です。"},
    {"text": ""},
]

CHUNK = [
    {"text": "我们今天讨论河流和城市的历史，以及它们如何随着时间改变。" * 6, "max_chars": 50},
    {"text": "这是一个没有任何标点符号的很长的句子它会一直继续下去直到超过最大长度为止" * 4, "max_chars": 40},
    {"text": "Ünïcödé wörds ärë hërë and they go on, ünïcödé wörds ärë hërë and they go on, " * 4, "max_chars": 60},
    {"text": "Visit " + "averyveryverylongtokenwithoutanyspaces" * 5 + " today.", "max_chars": 50},
    {"text": "Short. " + "x" * 120, "max_chars": 40},
    {"text": "First sentence is here. Then a tiny one. Ok.", "max_chars": 30},
    {"text": "It said \"Stop.\" Then it went on. (A note.) [Ref.] 3 apples.", "max_chars": 220},
    {"text": "We met Prof. Plum, Dr. Who and Mrs. Peacock etc. They left. The U.S. team won.", "max_chars": 220},
    {"text": "J. R. R. Tolkien wrote it. A. B. went home. I. Am. Here.", "max_chars": 220},
    {"text": "Line one\nLine two\n\n\nLine three  \n   \nLine four", "max_chars": 220},
    {"text": "Clause one is here, clause two is here; clause three is here: and clause four is here.", "max_chars": 30},
    {"text": "Words with nbsp go here and keep going. Another sentence.", "max_chars": 25},
    {"text": "¿Qué tal? ¡Muy bien! Él dijo: «sí». Ça va? Oui.", "max_chars": 20},
    {"text": "Ends with question? yes lower case. 99 bottles. 'Quoted' start.", "max_chars": 220},
    {"text": "", "max_chars": 220},
    {"text": "A" * 500, "max_chars": 220},
    {"text": "word " * 100, "max_chars": 220},
]

WORD_COUNT = [
    "", "Hello world", "It's Ender's game", "我们今天讨论河流", "日本語のテキスト", "한국어 문장",
    "mixed 中文 and English", "under_score words", "naïve café", "x² and ½ and ①",
    "1,000 people at 3.5 pm", "don't-stop", "   ", "’curly’ apostrophes", "Ünïcödé wörds",
    "emoji 😀 here", "a\u001cb", "ﾊﾝｶｸ half-width", "abc123 _ __ '",
]


def number_cases(n: int) -> list[str]:
    """Seeded random strings that walk the EnglishNumberNormalizer branches."""
    rng = random.Random(20261127)
    vocab = (
        "zero oh o one two three four five six seven eight nine ten eleven twelve thirteen nineteen "
        "twenty thirty forty fifty ninety hundred thousand million billion trillion decillion "
        "first second third fourth fifth twelfth thirteenth twentieth thirtieth hundredth thousandth millionth "
        "ones twos sixes tens twenties hundreds thousands millions "
        "and and and point point double triple percent per cent cents dollars dollar pounds euros "
        "minus plus negative positive half a "
        "0 1 2 5 7 10 12 20 99 100 1000 1.5 0.5 3.0 007 2.25 $5 £3 €2.5 -4 +6 1st 2nd 21st 1960s 5th "
        "the apples mr won't it's colour"
    ).split()
    out = []
    for _ in range(n):
        k = rng.randint(1, 8)
        out.append(" ".join(rng.choice(vocab) for _ in range(k)))
    return out


def random_pairs(n: int, seed: int, alphabet: str, lo: int, hi: int) -> list[tuple[str, str]]:
    """Seeded random string pairs over a small alphabet, so difflib hits many ties."""
    rng = random.Random(seed)
    word = lambda: "".join(rng.choice(alphabet) for _ in range(rng.randint(lo, hi)))
    return [(word(), word()) for _ in range(n)]


def random_sentences(n: int, seed: int) -> list[tuple[str, str]]:
    """Seeded script/transcript pairs: drop, swap, split and misspell words."""
    rng = random.Random(seed)
    vocab = "the a home come long day over they asked me to tree stood tall skadu scadu twenty 20 five $5 one won't".split()
    out = []
    for _ in range(n):
        ref = [rng.choice(vocab) for _ in range(rng.randint(1, 9))]
        hyp = list(ref)
        for _ in range(rng.randint(0, 3)):
            op = rng.randrange(4)
            if op == 0 and hyp:
                del hyp[rng.randrange(len(hyp))]
            elif op == 1:
                hyp.insert(rng.randint(0, len(hyp)), rng.choice(vocab))
            elif op == 2 and hyp:
                i = rng.randrange(len(hyp))
                hyp[i] = hyp[i][:-1] or "x"
            elif op == 3 and hyp:
                i = rng.randrange(len(hyp))
                w = hyp[i]
                hyp[i:i + 1] = [w[: len(w) // 2], w[len(w) // 2:]] if len(w) > 1 else [w]
        out.append((" ".join(ref) + ".", " ".join(hyp)))
    return out


# ---- step S04: library store, settings and seeder -------------------------------------------

CLOCK0 = 1800000000.0
DAY = 86400.0


def pin_clock(db, start: float = CLOCK0, step: float = 0.25) -> None:
    """db.py reads time.time(); a fixed, stepping clock makes every recorded file identical across runs."""
    t = [start]

    def now() -> float:
        t[0] += step
        return t[0]

    db.time = types.SimpleNamespace(time=now)


def record_database(db) -> dict:
    """Synthetic documents written through db.py, then copied (SQLite backup API) to rust/golden/library.sqlite3."""
    assert db.DB_PATH.parent.name.startswith("taketwo-golden."), db.DB_PATH
    assert db.list_profiles() == [] and db.list_generations() == []
    pin_clock(db)
    odd = {"zeta": 1, "alpha": [0.1, 1e-07, 1e22, 1e16, -0.0, 5e-324, 1.7976931348623157e308, 2.5, 3.0],
           "ints": [0, -1, 2**63 - 1, -(2**63), 2**64 - 1], "text": "Café ✓ \U0001F600 \"q\" \\ \u0000\t\n end",
           "empty": {}, "none": None, "flags": [True, False], "nested": {"b": {"d": 1, "c": [{"y": 2, "x": 1}]}, "a": ""}}
    for p in [
        {"id": "vlib01", "name": "Library Voice", "language": "en", "status": "ready", "pitch_hz": 182.25,
         "speech_rate_wps": 2.6, "studio": True, "created": 1700000000.0, "extra": odd},
        {"id": "vlib02", "name": "Analyzing Voice", "language": "de", "status": "analyzing"},  # no created: set from the clock
        {"id": "vlib03", "name": "Queued Voice", "status": "queued", "created": 1700000100.5},
        {"id": "vlib04", "name": "Broken Voice", "status": "error", "error": "Old error", "created": 1700000200.0},
        {"id": "vlib05", "name": "No status voice", "created": 1700000300.0},
    ]:
        db.put_profile(p)
    chunk = lambda i, st, **kw: {"i": i, "text": f"Part {i} text.", "status": st, **kw}
    for g in [
        {"id": "glib01", "profile_id": "vlib01", "status": "running", "title": "Running one", "created": 1700001000.0,
         "chunks": [chunk(0, "done", file="chunk_000.wav"), chunk(1, "running"), chunk(2, "pending"), chunk(3, "running")]},
        {"id": "glib02", "profile_id": "vlib01", "status": "queued", "title": "Queued one", "created": 1700001100.0,
         "chunks": [chunk(0, "pending")]},
        {"id": "glib03", "profile_id": "vlib01", "status": "done", "title": "Done one", "favorite": False,
         "created": 1700001200.0, "duration": 4.0, "file": "final.wav", "chunks": [chunk(0, "done", file="chunk_000.wav")], "extra": odd},
        {"id": "glib04", "profile_id": "vlib02", "status": "done", "created": 1700001300.0, "chunks": [],
         "deleted_at": CLOCK0 - 8 * DAY},  # soft-deleted more than 7 days before the pinned clock
        {"id": "glib05", "profile_id": "vlib02", "status": "done", "created": 1700001400.0, "chunks": []},
        {"id": "glib06", "profile_id": "vlib03", "status": "cancelled", "created": 1700001500.0, "chunks": [chunk(0, "pending")]},
        {"id": "glib07", "status": "error", "message": "No voice", "created": 1700001600.0, "chunks": [], "deleted_at": 0},
        {"id": "glib08", "profile_id": "vlib01", "status": "interrupted", "chunks": [chunk(0, "running")]},  # no created
    ]:
        db.put_generation(g)
    assert db.patch_generation_meta("glib03", {"title": "Renamed ✓", "favorite": True, "status": "ignored"})
    assert db.set_generation_deleted("glib05", True)
    assert db.set_generation_deleted("glib06", True) and db.set_generation_deleted("glib06", False)
    live = db.get_generation("glib02")
    live["progress"], live["title"] = 0.5, "stale title"
    assert db.put_generation_live(live)
    assert not db.put_generation_live({"id": "missing01", "status": "done"})
    with db._lock:
        rows = {t: [list(r) for r in db._db.execute(f"SELECT * FROM {t} ORDER BY id")] for t in ("profiles", "generations")}
    for t in rows:
        for r in rows[t]:
            r.pop()  # the doc column; its parsed copy is recorded below
    if LIB_DB.exists():
        LIB_DB.unlink()
    dst = sqlite3.connect(LIB_DB)
    with db._lock:
        db._db.backup(dst)
    dst.close()
    return {"file": LIB_DB.name, "clock_start": CLOCK0, "profiles": db.list_profiles(),
            "generations": db.list_generations(), "columns": rows}


def _outcome(fn, *args):
    try:
        return {"ok": fn(*args)}
    except Exception as e:  # the class is recorded too, so anything other than ValueError shows
        return {"error_type": type(e).__name__, "error": str(e)}


def checked_patches(db) -> list[dict]:
    import math

    pats = []
    for k, (lo, hi, typ) in db.SETTING_RULES.items():
        if typ is int:
            vals = [lo, hi, lo - 1, hi + 1, lo + 0.7, hi + 0.5, lo - 0.5, str(lo), str(hi + 1), float(hi)]
        else:
            vals = [lo, hi, math.nextafter(lo, -math.inf), math.nextafter(hi, math.inf), math.nextafter(lo, math.inf),
                    math.nextafter(hi, -math.inf), lo - 1e-9, hi + 1e-9, repr(lo), repr(hi), "nan", "inf", "-inf", "1e999"]
            if float(lo).is_integer():
                vals.append(int(lo))
            if float(hi).is_integer():
                vals.append(int(hi))
        vals += [True, False, None, "", "abc", [], {}]
        pats += [{k: v} for v in vals]
    pats += [{"compare_takes": v} for v in [
        " 2 ", "+2", "02", "0_2", "1__2", "_2", "2_", "2.0", "2e0", "0x2", "2 3", "\t2\n", " 2 ", "٣", "２",
        "-0", "١", "2.9", 2.9, 3.99, 0.99, -0.99, 1e300, -1e300, "nan", "inf", "Infinity"]]
    pats += [{"max_wer": v} for v in [
        "0.1", " .1 ", "1e-1", "1E-1", "+0.1", "0.1_0", "0._1", "0_.1", "٠.١", "NaN", "-nan", "+Infinity",
        "infinity", "-1e999", "0.1f", " ", "0,1", "1_0e-2", "1e-0_1", ".5", "5.", ".", "e1", "0.12\n"]]
    pats += [{"seed": v} for v in ["2147483647", "2147483648", "99999999999999999999", 2**31, 2**31 - 1, -1, "-0", 2**63, 2**64 - 1]]
    pats += [{"check_updates": v} for v in [True, False, 1, 0, "true", None]]
    pats += [{"update_dismissed": v} for v in [
        "", "2.1.0", "0.0.0", "1234.1234.1234", "12345.1.1", "2.1", "2.1.0.1", "v2.1.0", " 2.1.0", "2.1.0\n",
        "٢.١.٠", "2..1", 210, 2.1, None, False]]
    pats += [{"custom_engines": v} for v in [
        [], [{"id": "a", "repo": "b/c"}], [{"id": "a", "repo": "b", "name": "x"}], [{"id": "a"}], [{"id": 1, "repo": "b"}],
        [{"id": "a", "repo": None}], [{"id": "a", "repo": "b"}, 1], "x", [1], {"id": "a", "repo": "b"}, None]]
    pats += [{"zzz": 1}, {"pronunciations": [{"from": "a", "to": "b"}]}, {"default_engine": 5}, {"strip_citations": "no"},
             {}, {"max_attempts": 3, "max_wer": 2, "check_updates": 1}, {"check_updates": 1, "max_wer": 2},
             {"zzz": 1, "max_attempts": "4", "target_lufs": -16}, {"max_wer": "0.2", "update_dismissed": "x"}]
    return pats


SETTINGS_FILES = [
    None, "{}", '{"max_attempts": 5, "zzz": 1}', '{"custom_engines": [{"id": "a"}], "seed": 7}',
    '{"custom_engines": [{"id": "a", "repo": "b"}]}', '{"custom_engines": "bad", "b": 1}', "", "{", "[1, 2]", "null", '"x"',
    "﻿{}", b"\xff\xfe{}", '{"seed": 1, "max_attempts": 2, "seed": 3}', '{"b": 1, "a": 2, "b": 3}', '{"seed": "abc"}',
    '  {"seed": 5}\n\n', '{"seed": 5} x', '{"custom_engines": null}',
]

SAVES = [
    (None, {"max_attempts": "5"}), (None, {"zzz": 1, "seed": 9}), ('{"zzz": 1}', {"seed": 9}), (None, {"max_wer": 9}),
    ("{broken", {"seed": 1}), ("{broken", {"seed": -1}), ('{"custom_engines": [1]}', {}), (None, {}),
    (None, {"target_lufs": -16}), ('{"seed": 5}', {"seed": 6, "check_updates": "x"}),
    (None, {"custom_engines": [{"id": "m", "repo": "org/model"}], "pronunciations": [{"from": "SQL", "to": "sequel"}]}),
]


def _settings_file(config, content) -> dict:
    for n in ("settings.json", "settings.json.broken", "settings.json.tmp"):
        (config.DATA_DIR / n).unlink(missing_ok=True)
    if content is None:
        return {"file": None}
    if isinstance(content, bytes):
        config.SETTINGS_PATH.write_bytes(content)
        return {"file_hex": content.hex()}
    config.SETTINGS_PATH.write_text(content)
    return {"file": content}


def _settings_after(config) -> dict:
    p = config.SETTINGS_PATH
    return {"settings_exists": p.exists(), "broken_exists": p.with_name("settings.json.broken").exists(),
            "tmp_exists": p.with_name("settings.json.tmp").exists(),
            "file_after": json.loads(p.read_text()) if p.exists() else None}


def record_settings(db, config) -> dict:
    assert config.SETTINGS_PATH.parent.name.startswith("taketwo-golden."), config.SETTINGS_PATH
    checked = [{"patch": p, **_outcome(db._checked, p)} for p in checked_patches(db)]
    gets = []
    for f in SETTINGS_FILES:
        case = _settings_file(config, f)
        gets.append({**case, **_outcome(db.get_settings), **_settings_after(config)})
    saves = []
    for f, patch in SAVES:
        case = _settings_file(config, f)
        saves.append({**case, "patch": patch, **_outcome(db.save_settings, patch), **_settings_after(config)})
    _settings_file(config, None)
    rules = [{"key": k, "lo": lo, "hi": hi, "type": t.__name__} for k, (lo, hi, t) in db.SETTING_RULES.items()]
    return {"defaults": [config.DEFAULT_SETTINGS], "rules": rules, "gen_meta_keys": [list(db.GEN_META_KEYS)],
            "checked": checked, "get_settings": gets, "save_settings": saves}


def seed_golden() -> list[dict]:
    """Runs seed_python.seed() in a child process: db.py opens one database at import, so the seed gets its own."""
    tmp = Path(tempfile.mkdtemp(prefix="taketwo-seed."))
    try:
        env = {**os.environ, "TAKETWO_DATA": str(tmp), "VOICEFORGE_DATA": str(tmp), "HF_HUB_OFFLINE": "1"}
        out = subprocess.run([sys.executable, "-B", str(Path(__file__).resolve()), "--seed-golden"],
                             env=env, check=True, capture_output=True, text=True).stdout
        return [json.loads(out.strip().splitlines()[-1])]
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def seed_child() -> None:
    data = Path(os.environ["TAKETWO_DATA"])
    assert data.name.startswith("taketwo-seed.") and os.environ["VOICEFORGE_DATA"] == str(data), data
    sys.path.insert(0, str(BACKEND))
    from app import config

    assert config.DATA_DIR.resolve() == data.resolve(), config.DATA_DIR  # never the real library
    from app import db

    pin_clock(db)
    sys.path.insert(0, str(WT / "contract"))
    import seed_python

    seed_python.seed(data)
    files = []
    for f in sorted(data.rglob("*")):
        rel = f.relative_to(data).as_posix()
        if f.is_dir():
            files.append({"path": rel + "/"})
        elif f.suffix == ".wav":
            with wave.open(str(f)) as w:
                head = list(struct.unpack("<32h", w.readframes(32)))  # catches a wrong frequency or amplitude
                files.append({"path": rel, "channels": w.getnchannels(), "sample_width": w.getsampwidth(),
                              "rate": w.getframerate(), "frames": w.getnframes(), "bytes": f.stat().st_size,
                              "first_samples": head})
        else:
            files.append({"path": rel})
    print(json.dumps({"files": files, "profiles": db.list_profiles(), "generations": db.list_generations()}, allow_nan=False))


# ---- step S05: captions, timings, subtitles, review rules ---------------------------------------

REVIEW_OUT = HERE / "review.json"
NOW0 = 1800001234.5


def _w(word, start, end):
    return {"word": word, "start": start, "end": end}


def _heard(text, start=0.0, step=0.25):
    """Heard words for a script, one per step."""
    return [_w(x, round(start + n * step, 6), round(start + (n + 1) * step, 6)) for n, x in enumerate(text.split())]


def _gen(chunks, timeline, **extra):
    return {"id": "s05gen", "status": "done", **extra, "timeline": timeline,
            "chunks": [{"i": n, **c} for n, c in enumerate(chunks)]}


def _run(fn, *args):
    try:
        return {"result": fn(*args)}
    except Exception as e:  # noqa: BLE001 - the Rust port must fail the same cases
        return {"error": type(e).__name__}


LONG_EN = ("This sentence is deliberately long, with several clauses; it keeps going: on and on, until it is far "
           "longer than the caption limit allows. Short one. And a final question that is also rather long for one "
           "caption, wouldn't you say, my friend?")


def timing_cases() -> list[dict]:
    t1 = "Hello there, my friend. How are you?"
    cases = [
        _gen([{"text": t1, "words": _heard(t1)}], [{"i": 0, "start": 0.0, "end": 2.0}], speed=1.0, duration=2.0),
        _gen([{"text": "The cat sat down.", "words": _heard("The dog sat down.")}], [{"i": 0, "start": 0.5, "end": 1.6}]),
        _gen([{"text": "Well I think so.", "words": _heard("I think so.", 0.3)}], [{"i": 0, "start": 1.0, "end": 2.2}], speed=0.8),
        _gen([{"text": "We go home now.", "words": _heard("We go home")}], [{"i": 0, "start": 0.0, "end": 1.0}], speed=1.5),
        _gen([{"text": "We go to the store.", "words": _heard("Um we go to the the store")}], [{"i": 0, "start": 0.0, "end": 2.0}], speed=0),
        _gen([{"text": "One two three four five.", "words": _heard("one three five")}], [{"i": 0, "start": 0.2, "end": 1.4}], speed=None),
        _gen([{"text": "Don't STOP, it's Franz's turn!", "words": _heard("dont stop its franz's TURN")}], [{"i": 0, "start": 0.0, "end": 1.5}]),
        _gen([{"text": "Café crème, naïve Æsop.", "words": _heard("cafe creme naive æsop")}], [{"i": 0, "start": 0.0, "end": 1.2}]),
        # a part with words beside one without (estimated)
        _gen([{"text": "First part here.", "words": _heard("First part here.")},
              {"text": "Second part has no word times at all."}],
             [{"i": 0, "start": 0.0, "end": 0.75}, {"i": 1, "start": 1.1, "end": 3.3}], speed=1.25, duration=3.3),
        # empty part text, and sentences with no words
        _gen([{"text": ""}, {"text": "   "}, {"text": "?! Hello. . World."}],
             [{"i": 0, "start": 0.0, "end": 0.5}, {"i": 1, "start": 0.5, "end": 1.0}, {"i": 2, "start": 1.0, "end": 2.0}]),
        _gen([{"text": "", "words": [_w("ghost", 0.0, 0.2)]}], [{"i": 0, "start": 0.0, "end": 0.5}]),
        # integer times stay integers where Python keeps them
        _gen([{"text": "Integer start and end times."}], [{"i": 0, "start": 0, "end": 3}], duration=3),
        _gen([{"text": "Integer word times.", "words": [_w("Integer", 0, 1), _w("word", 1, 2), _w("times.", 2, 3)]}],
             [{"i": 0, "start": 1, "end": 4}]),
        # half milliseconds and times above one hour
        _gen([{"text": "Half a milli.", "words": [_w("Half", 0.0005, 0.0015), _w("a", 0.0015, 0.0025), _w("milli.", 0.0025, 0.0045)]}],
             [{"i": 0, "start": 0.0, "end": 0.01}]),
        _gen([{"text": "Later on.", "words": [_w("Later", 0.0005, 0.5), _w("on.", 0.5, 0.9995)]}],
             [{"i": 0, "start": 2.5, "end": 3.5}]),
        _gen([{"text": "Almost an hour.", "words": [_w("Almost", 0.0, 0.0005), _w("an", 0.0005, 0.0015), _w("hour.", 0.0015, 0.0025)]}],
             [{"i": 0, "start": 3599.999, "end": 3600.001}]),
        _gen([{"text": "Well past one hour now."}], [{"i": 0, "start": 3725.2505, "end": 3731.0015}]),
        _gen([{"text": "Two hours in."}], [{"i": 0, "start": 7200.0005, "end": 7201.9995}]),
        # long text: caption pieces inside a timeline
        _gen([{"text": LONG_EN}], [{"i": 0, "start": 0.0, "end": 14.3}]),
        _gen([{"text": "这是一个很长的中文句子，没有空格，所以必须按字符切分。" * 4}], [{"i": 0, "start": 0.0, "end": 9.0}]),
        # duplicate chunk ids: the last one wins in the {i: chunk} map
        {"id": "s05dup", "status": "done", "timeline": [{"i": 0, "start": 0.0, "end": 1.0}],
         "chunks": [{"i": 0, "text": "First copy."}, {"i": 0, "text": "Second copy wins."}]},
        # timeline order is the order of the output, whatever the part numbers
        _gen([{"text": "Part zero."}, {"text": "Part one."}], [{"i": 1, "start": 0.0, "end": 0.8}, {"i": 0, "start": 1.0, "end": 1.7}]),
        {"id": "s05none", "status": "done", "chunks": [{"i": 0, "text": "No timeline."}]},
        _gen([{"text": "Speed as text.", "words": _heard("Speed as text.")}], [{"i": 0, "start": 0.0, "end": 1.0}], speed="1.5"),
        # Python raises: a timeline entry for a part that does not exist, a null timeline
        _gen([{"text": "Only part."}], [{"i": 3, "start": 0.0, "end": 1.0}]),
        {"id": "s05null", "status": "done", "timeline": None, "chunks": [{"i": 0, "text": "Null timeline."}]},
    ]
    return cases


ROUND = [0.0, -0.0, 0.0005, 0.0015, 0.0025, 0.0035, 0.0045, 0.1235, 1.0005, 2.5005, 2.675, 3599.9995, 3600.0005,
         7200.0015, 86399.9995, 0.0625, 0.1875, 0.3125, 0.4375, 1.0625, 1e16, 1.5e300, 123456.7895, -0.0005,
         -2.5005, 5e-324, 0.000499999, 0.00050001, 1 / 3, 2 / 3]


def round_cases() -> list[float]:
    rnd = random.Random(23)
    extra = [rnd.randrange(0, 8_000_000) / 2000 for _ in range(150)] + [rnd.random() * 10_000 for _ in range(100)]
    return ROUND + extra


WRAP = [
    "x" * 41, "x" * 42, "x" * 43,
    "This line has exactly forty-one chars ok.", "This line has exactly forty-two chars, ok.",
    "This line has exactly forty-three chars, ok.",
    "averyveryverylongtokenwithoutanyspacesatallthatkeepsgoingandgoing",
    "这是一个很长的中文句子没有空格所以必须按字符切分这是一个很长的中文句子没有空格",
    "这是一个很长的中文句子没有空格所以必须按字符切分这是一个很长的中文句子没有空格好",
    "这是一个很长的中文句子没有空格所以必须按字符切分" * 2 + "好", "这是一个很长的中文句子没有空格所以必须按字符切分" * 2 + "好的",
    "Supercalifragilisticexpialidocious-and-even-longer-word a b",
    "aaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "Welcome to the show & thanks for listening, today we talk about rivers.",
    "many   spaces    between   these words    and more words here",
    "Ça va très bien, merci beaucoup, et vous, mon ami très cher?",
    "a b c d e f g h i j k l m n o p q r s t u v w x y z a b c",
]

PIECES = [
    "", "   \n\t ", "One. Two! Three? Four", "Hi. There.\x1cNext. Done",
    "a" * 84, "a" * 85, ("word " * 17).strip()[:84], ("word " * 17).strip()[:85] + "x",
    ("abcd " * 17).strip(), ("abcd " * 17).strip() + "e",
    "The first clause of this sentence is long enough to pass the limit, then a second clause follows here and ends.",
    "Short clause, then a much much longer clause that goes well past the flush limit of the captions here; end: x y.",
    "Wörter mit Umlauten über Äpfel, Öl und Übermut sind länger in Bytes als in Zeichen, aber nicht zu lang.",
    "Wörter mit Umlauten über Äpfel, Öl und Übermut sind länger in Bytes als in Zeichen, aber gar nicht zu lang hier.",
    "这是一个很长的中文句子，没有空格，所以必须按字符切分。" * 4,
    "x" * 100 + " tail words after a huge one.",
    LONG_EN,
    "Line one.\n\nLine two?  Line three!   ",
]

HEADERS = [
    ("Rivers and towns", "wav"), ("Café olé", "mp3"), ("日本語のタイトル", "srt"), ('He said "hi"', "vtt"),
    ("back\\slash", "txt"), ("line\r\nbreak", "wav"), ("  spaced  out  ", "mp3"), ("100% / done", "wav"),
    ("", "wav"), ("~tilde_under-score.dot", "srt"), ("🎙 Mic check", "mp3"), ("tab\there", "wav"),
    ("a+b=c&d?e#f", "txt"), ("  日本  ", "vtt"),
]


def _chunk(text="A part.", **kw):
    return {"text": text, "status": "ok", "issues": [], "file": "x.wav", "heard": text, "pitch_hz": 180.0, **kw}


def review_cases() -> list[dict]:
    tl3 = [{"i": 0, "start": 0.0, "end": 1.0}, {"i": 1, "start": 1.2, "end": 2.0}, {"i": 2, "start": 2.4, "end": 3.0}]
    g = lambda chunks, **kw: {"id": "s05rev", "status": "done", "flagged": [], "message": "old", **kw,
                              "chunks": [{"i": n, **c} for n, c in enumerate(chunks)]}
    flagged = lambda text="Flagged part.", codes=("cut_off",), **kw: _chunk(text, status="flagged", issues=list(codes), **kw)
    return [
        g([_chunk(), _chunk(), _chunk()], timeline=tl3),
        g([_chunk(), flagged(), _chunk()], timeline=tl3),
        g([_chunk(), _chunk(), _chunk(final_issues=["processed_audio_missing"])], timeline=tl3, final_scan={"remaining": [2]}),
        g([_chunk(), _chunk("It&39;s web code."), _chunk()], timeline=tl3),
        g([_chunk(), flagged(accepted={"by": "person", "at": 1.5}), _chunk()], timeline=tl3),
        g([flagged(accepted={"by": "person", "at": 1.5}), flagged(accepted=True), _chunk()], timeline=tl3),
        g([flagged(), _chunk(), flagged("Third &amp; last."), _chunk(), flagged(codes=("too_long",))], timeline=tl3),
        g([flagged(codes=("cut_off", "leak"), final_issues=["leak", "noise", "cut_off"])], timeline=tl3),
        g([flagged(codes=())], timeline=tl3),
        g([flagged(codes=("weird_new_thing", "x"))], timeline=tl3),
        g([flagged()]),
        g([flagged(take=3), flagged(take=None), flagged()], timeline=tl3),
        g([flagged(), _chunk()], timeline=tl3, status="rendering"),
        g([flagged(heard=None, pitch_hz=None)], timeline=tl3),
        {"id": "s05bare", "status": "done", "chunks": [{"i": 0, "text": "Bare.", "status": "flagged"}]},
        g([flagged(codes=("web_artifact",), text="It&#39;s here.")], timeline=tl3),
        g([_chunk(accepted={"by": "person", "at": 2.0}), _chunk()], timeline=tl3, final_scan={"remaining": [0, 1]}),
        g([_chunk(), _chunk()], timeline=tl3, final_scan=None),
        g([_chunk(), _chunk()], timeline=tl3, final_scan={"remaining": None}),
        g([flagged(text=None), _chunk()], timeline=tl3),
        g([flagged(accepted={}), _chunk()], timeline=tl3),
        g([flagged(), flagged()], timeline=[{"i": 1, "start": 0.0, "end": 1.0}, {"i": 1, "start": 5.0, "end": 6.0}, {"i": 0, "start": 7, "end": 8}]),
        g([_chunk(), _chunk()], timeline=tl3, final_scan={"remaining": [1, 1, 0]}),
        {"id": "s05nokeys", "status": "done", "chunks": [{"i": 0, "text": "A.", "status": "ok"}, {"i": 1, "text": "B.", "status": "flagged"}]},
        g([_chunk(), _chunk()], timeline=tl3, final_scan={"remaining": [5]}),  # Python raises IndexError in review()
        g([_chunk(), flagged()], timeline=tl3, status="cancelled"),
    ]


def record_review_rules(pipeline) -> list[dict]:
    out = []
    for gen in review_cases():
        case = {"gen": copy.deepcopy(gen), "fix_targets": _run(pipeline.fix_targets, copy.deepcopy(gen)),
                "outcome": _run(pipeline.outcome, copy.deepcopy(gen))}
        for name, pitch in (("review", None), ("review_pitch", 181.25)):
            case[name] = _run(pipeline.review, copy.deepcopy(gen), pitch)
        summarized = copy.deepcopy(gen)
        case["summarize"] = _run(lambda d: (pipeline.summarize(d), d)[1], summarized)
        out.append(case)
    return out


def record_accept(pipeline) -> list[dict]:
    base = review_cases()
    picks = [(base[1], 1, True), (base[1], 1, False), (base[4], 1, False), (base[5], 2, True), (base[2], 2, True),
             (base[12], 0, True), (base[12], 0, False), (base[0], 0, True), (base[1], -1, True), (base[1], 7, True)]
    out = []
    for n, (gen, ci, on) in enumerate(picks):
        now = NOW0 + n * 0.125
        pipeline.time = types.SimpleNamespace(time=lambda now=now: now)
        out.append({"gen": copy.deepcopy(gen), "ci": ci, "on": on, "now": now,
                    "after": _run(pipeline.set_accepted, copy.deepcopy(gen), ci, on)})
    return out


def _tree(root: Path) -> dict:
    return {f.relative_to(root).as_posix() + ("/" if f.is_dir() else ""): (f.read_text() if f.is_file() else None)
            for f in sorted(root.rglob("*"))}


def record_takes(pipeline, gens: Path) -> list[dict]:
    full = {"i": 1, "text": "Take me.", "file": "chunk_001.wav", "raw_file": "chunk_001.raw.wav",
            "context_file": "chunk_001.ctx.wav", "wer": 0.0, "sim": 0.91, "dur": 1.2, "issues": ["cut_off"],
            "final_issues": [], "heard": "Take me.", "pitch_hz": 180.0, "words": [_w("Take", 0.0, 0.3)],
            "ending": {"ok": True}, "status": "flagged", "chosen_seed": 7, "accepted": {"by": "person", "at": 1.0},
            "take": 2, "pause_after": "end"}
    three = {"chunk_001.wav": "a1", "chunk_001.raw.wav": "r1", "chunk_001.ctx.wav": "c1"}
    old = lambda *ks: [{"take": k, "text": f"take {k}"} for k in ks]
    takes_files = lambda gid, *ks: {f"takes/001-{k}/chunk_001.wav": f"old {k}" for k in ks}
    specs = [
        ("all three files", full, three),
        ("context file missing on disk", full, {"chunk_001.wav": "a1", "chunk_001.raw.wav": "r1"}),
        ("audio file missing", full, {"chunk_001.raw.wav": "r1"}),
        ("no file key", {k: v for k, v in full.items() if k != "file"}, three),
        ("take missing, minimal part", {"i": 1, "file": "chunk_001.wav"}, {"chunk_001.wav": "a1"}),
        ("take as float", {**full, "take": 2.0, "raw_file": None, "context_file": ""}, three),
        ("five old takes", {**full, "take": 6, "takes": old(1, 2, 3, 4, 5)}, {**three, **takes_files("g", 1, 2, 3, 4, 5)}),
        ("two old takes", {**full, "take": 3, "takes": old(1, 2)}, {**three, **takes_files("g", 1, 2)}),
        ("four old takes, part 0", {**full, "i": 0, "file": "chunk_000.wav", "take": 5, "takes": old(1, 2, 3, 4)},
         {"chunk_000.wav": "z", "takes/000-1/chunk_000.wav": "o1", "takes/000-2/chunk_000.wav": "o2", "takes/000-4/chunk_000.wav": "o4"}),
    ]
    out = []
    for n, (name, ch, files) in enumerate(specs):
        gid = f"s05take{n}"
        gdir = gens / gid
        for rel, content in files.items():
            (gdir / rel).parent.mkdir(parents=True, exist_ok=True)
            (gdir / rel).write_text(content)
        now = NOW0 + 100 + n
        pipeline.time = types.SimpleNamespace(time=lambda now=now: now)
        before = copy.deepcopy(ch)
        archived = pipeline._archive_take(gid, ch)
        trimmed = copy.deepcopy(ch)
        pipeline._trim_takes(gid, trimmed)
        gen = {"id": gid, "chunks": [{"i": 0, "take": 1}, trimmed] if trimmed["i"] == 1 else [trimmed]}
        ci = trimmed["i"]
        audio = {str(k): (lambda p: p.relative_to(gens).as_posix() if p else None)(pipeline.take_audio(gen, ci, k)) for k in range(0, 8)}
        out.append({"name": name, "gen_id": gid, "chunk": before, "files": files, "now": now, "archived": archived,
                    "trimmed": trimmed, "tree": _tree(gdir), "take_audio_ci": ci, "take_audio": audio})
        shutil.rmtree(gdir)
    return out


def record_review(config) -> dict:
    """tools.py captions and timings, pipeline.py review rules and take bookkeeping, on synthetic documents."""
    from app import pipeline, tools

    saved_time = pipeline.time
    try:
        data = {
            "issue_label": [[k, v] for k, v in pipeline.ISSUE_LABEL.items()],
            "take_fields": list(pipeline.TAKE_FIELDS),
            "max_old_takes": pipeline.MAX_OLD_TAKES,
            "max_caption_chars": tools.MAX_CAPTION_CHARS,
            "round": [{"x": x, "round3": round(x, 3), "ts": tools._ts(x, ",") if abs(x) < 1e15 else None} for x in round_cases()],
            "wrap": [{"text": t, "out": tools._wrap(t)} for t in WRAP],
            "pieces": [{"text": t, "pieces": tools._caption_pieces(t)} for t in PIECES],
            "header": [{"name": n, "ext": e, "out": tools.attachment_header(n, e)} for n, e in HEADERS],
            "timings": [],
            "rules": record_review_rules(pipeline),
            "accept": record_accept(pipeline),
            "takes": record_takes(pipeline, config.GENERATIONS_DIR),
        }
        for gen in timing_cases():
            case = {"gen": copy.deepcopy(gen), "timings": _run(tools.generation_timings, copy.deepcopy(gen))}
            caps = _run(tools.generation_captions, copy.deepcopy(gen))
            case["captions"] = caps
            if "result" in caps:
                c = caps["result"]
                case.update(srt=tools.to_srt(c), vtt=tools.to_vtt(c), txt=tools.to_txt(c))
            data["timings"].append(case)
    finally:
        pipeline.time = saved_time
    assert len(data["issue_label"]) == 25, len(data["issue_label"])
    return data


def main() -> None:
    tmp = Path(tempfile.mkdtemp(prefix="taketwo-golden."))
    try:
        os.environ["TAKETWO_DATA"] = os.environ["VOICEFORGE_DATA"] = str(tmp)
        os.environ["HF_HUB_OFFLINE"] = os.environ["TRANSFORMERS_OFFLINE"] = "1"
        sys.path.insert(0, str(BACKEND))
        from app import config

        assert config.DATA_DIR.resolve().is_relative_to(tmp.resolve()), config.DATA_DIR  # never the real library

        from transformers import WhisperTokenizer

        from app import ml, pipeline, textproc

        def no_model():
            raise RuntimeError("record.py must never load a model")

        ml.stt_model = no_model
        tok = WhisperTokenizer.from_pretrained("openai/whisper-large-v3-turbo", local_files_only=True)
        assert tok.english_spelling_normalizer == json.loads(SPELLING.read_text()), "spelling map drifted"
        ml._normalizer = tok.normalize

        data = {
            "normalize": [{"text": t, "output": ml.normalize_for_wer(t)} for t in NORMALIZE + number_cases(300)],
            "wer": [{"reference": r, "hypothesis": h, "wer": ml.wer(r, h)} for r, h in WER + random_sentences(60, 7)],
            "ratio": [{"a": a, "b": b, "ratio": difflib.SequenceMatcher(None, a, b).ratio()} for a, b in RATIO + random_pairs(150, 11, "abc", 0, 12) + random_pairs(20, 13, "abcde", 190, 320)],
            "coverage": [
                {
                    "text": t,
                    "heard": h,
                    "covered": pipeline._word_coverage(t, h),
                    "final_missing": pipeline._final_word_missing(t, h),
                    "first_missing": pipeline._first_word_missing(t, h),
                }
                for t, h in COVERAGE + random_sentences(80, 17)
            ],
            "clean_text": [
                {**c, "output": textproc.clean_text(c["text"], c.get("pronunciations"), c.get("strip_citations", True))}
                for c in CLEAN
            ],
            "chunk_text": [
                {**c, "chunks": [{"text": k.text, "pause_after": k.pause_after} for k in textproc.chunk_text(c["text"], c["max_chars"])]}
                for c in CHUNK
            ],
            "word_count": [{"text": t, "count": textproc.word_count(t)} for t in WORD_COUNT],
        }
        OUT.write_text(json.dumps(data, ensure_ascii=True, indent=1) + "\n")
        print({k: len(v) for k, v in data.items()})

        from app import db

        lib = {"database": record_database(db), **record_settings(db, config), "seed": seed_golden()}
        LIB_OUT.write_text(json.dumps(lib, ensure_ascii=True, indent=1, allow_nan=False) + "\n")
        print({k: len(v) for k, v in lib.items()})

        rev = record_review(config)
        REVIEW_OUT.write_text(json.dumps(rev, ensure_ascii=True, indent=1, allow_nan=False) + "\n")
        print({k: (len(v) if isinstance(v, list) else v) for k, v in rev.items()})
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    if sys.argv[1:] == ["--seed-golden"]:
        seed_child()
    else:
        main()
