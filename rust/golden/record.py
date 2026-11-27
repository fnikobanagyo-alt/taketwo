"""Record Python parity vectors for the Rust `text` module (step S03).

    backend/.venv/bin/python -B rust/golden/record.py

Writes rust/golden/text.json: synthetic inputs and what the Python app returns for them
(textproc, ml.normalize_for_wer, ml.wer, difflib ratio, pipeline word coverage).
Safety: the data folder is a fresh temporary folder (checked before any other app import),
Hugging Face is offline, and the Whisper normalizer comes from tokenizer files only.
No model is loaded or downloaded: ml.stt_model is replaced by a function that raises.
"""
from __future__ import annotations

import difflib
import json
import os
import random
import shutil
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
BACKEND = WT / "backend"
OUT = HERE / "text.json"
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
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
