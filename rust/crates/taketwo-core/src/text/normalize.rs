//! `ml.normalize_for_wer`: a port of transformers' Whisper `EnglishTextNormalizer`
//! (models/whisper/english_normalizer.py, itself from openai/whisper), with the British to
//! American spelling map from the tokenizer's `normalizer.json` embedded as data.

use super::py;
use num_bigint::BigUint;
use regex::{Captures, Regex};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| py::re($pat));
    };
}

re!(BRACKETS, r"[<\[][^>\]]*[>\]]");
re!(PARENS, r"\(([^)]+?)\)");
re!(SPACE_APOSTROPHE, r"[{SPACE}]+'");
re!(DIGIT_COMMA, r"(\d),(\d)");
re!(PERIOD, r"\.([^0-9]|$)");
re!(PREFIX_SYMBOL, r"[.$¢€£]([^0-9])");
re!(SUFFIX_SYMBOL, r"([^0-9])%");
re!(SPACES, r"[{SPACE}]+");
re!(AND_A_HALF, r"and[{SPACE}]+a[{SPACE}]+half");
re!(LETTER_DIGIT, r"([a-z])([0-9])");
re!(DIGIT_LETTER, r"([0-9])([a-z])");
re!(NUMERIC, r"\A\d+(?:\.\d+)?\z");
re!(COMBINE_CENTS, r"([€£$])([0-9]+) (?:and )?¢([0-9]{1,2})");
re!(EXTRACT_CENTS, r"[€£$]0.([0-9]{1,2})");
re!(MN, r"\A\p{Mn}\z");
re!(MSP, r"\A[\p{M}\p{S}\p{P}]\z");

static SPELLING: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../data/whisper_normalizer.json")).expect("spelling map")
});

/// Replace each match of a pattern that starts at a literal and ends with Python's `\b`.
/// `alts` are tried in order at each position, as a regex alternation would be.
fn sub_words(s: &str, alts: &[&str], lead_b: bool, repl: impl Fn(&str) -> String) -> String {
    let c: Vec<char> = s.chars().collect();
    let alts: Vec<Vec<char>> = alts.iter().map(|a| a.chars().collect()).collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    'scan: while i < c.len() {
        if !lead_b || py::boundary(&c, i) {
            for a in &alts {
                let end = i + a.len();
                if c[i..].starts_with(a) && py::boundary(&c, end) {
                    out.push_str(&repl(&a.iter().collect::<String>()));
                    i = end;
                    continue 'scan;
                }
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `re.sub(pattern + r"\b", f, s)` for a pattern whose shorter matches can't end on a boundary.
fn sub_then_boundary(re: &Regex, s: &str, f: impl Fn(&Captures) -> String) -> String {
    let mut out = String::new();
    let (mut copied, mut pos) = (0, 0);
    while let Some(caps) = re.captures_at(s, pos) {
        let m = caps.get(0).unwrap();
        let next = s[m.end()..].chars().next();
        let prev = s[..m.end()].chars().next_back();
        if prev.is_some_and(py::is_word) != next.is_some_and(py::is_word) {
            out.push_str(&s[copied..m.start()]);
            out.push_str(&f(&caps));
            copied = m.end();
            pos = m.end();
        } else {
            pos = m.start() + s[m.start()..].chars().next().unwrap().len_utf8();
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// Non-ASCII letters that are not separated by NFKD normalization.
fn additional_diacritic(c: char) -> Option<&'static str> {
    Some(match c {
        'œ' => "oe",
        'Œ' => "OE",
        'ø' => "o",
        'Ø' => "O",
        'æ' => "ae",
        'Æ' => "AE",
        'ß' => "ss",
        'ẞ' => "SS",
        'đ' => "d",
        'Đ' => "D",
        'ð' => "d",
        'Ð' => "D",
        'þ' => "th",
        'Þ' => "th",
        'ł' => "l",
        'Ł' => "L",
        _ => return None,
    })
}

/// Replace markers, symbols and punctuation with a space and drop diacritics (category Mn).
pub fn remove_symbols_and_diacritics(s: &str, keep: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut buf = [0u8; 4];
    for c in s.nfkd() {
        if keep.contains(c) {
            out.push(c);
        } else if let Some(r) = additional_diacritic(c) {
            out.push_str(r);
        } else if c.is_ascii_alphanumeric() || c == ' ' {
            out.push(c);
        } else {
            let one = c.encode_utf8(&mut buf);
            if MN.is_match(one) {
            } else if MSP.is_match(one) {
                out.push(' ');
            } else {
                out.push(c);
            }
        }
    }
    out
}

#[derive(Clone, Debug)]
enum Value {
    Int(BigUint),
    Str(String),
}

impl Value {
    fn text(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Str(s) => s.clone(),
        }
    }
}

/// Python's `str(value or "")`: None, 0 and "" are falsy.
fn falsy_text(v: &Option<Value>) -> String {
    match v {
        Some(Value::Int(n)) if *n == BigUint::ZERO => String::new(),
        Some(v) => v.text(),
        None => String::new(),
    }
}

/// `Fraction(s)` for the strings this code can build: digits with an optional `.digits`
/// part ("3.", ".5", "007"). Returns (all digits as one integer, number of decimals).
/// Values never start with a sign or hold `/` or an exponent here, so those are not parsed.
fn to_fraction(s: &str) -> Option<(BigUint, u32)> {
    let (num, dec) = match s.split_once('.') {
        Some((n, d)) => (n, Some(d)),
        None => (s, None),
    };
    let digits = |t: &str| t.chars().all(|c| NUMERIC.is_match(c.encode_utf8(&mut [0u8; 4])));
    let starts_ok = num.chars().next().is_some() || dec.is_some_and(|d| !d.is_empty());
    if !starts_ok || !digits(num) || !dec.is_none_or(digits) {
        return None;
    }
    let all: String = num.chars().chain(dec.unwrap_or("").chars()).map(|c| char::from(b'0' + py::digit_value(c) as u8)).collect();
    let n = if all.is_empty() { BigUint::ZERO } else { all.parse().unwrap() };
    Some((n, dec.map_or(0, |d| py::len(d) as u32)))
}

fn pow10(k: u32) -> BigUint {
    BigUint::from(10u32).pow(k)
}

/// `f * 10^k` when it is a whole number.
fn times_pow10(f: &(BigUint, u32), k: u32) -> Option<BigUint> {
    let (n, d) = f;
    if *d <= k {
        Some(n * pow10(k - d))
    } else {
        let div = pow10(d - k);
        (n % &div == BigUint::ZERO).then(|| n / div)
    }
}

struct Numbers {
    zeros: HashSet<&'static str>,
    ones: HashMap<String, u32>,
    ones_suffixed: HashMap<String, (u32, &'static str)>,
    tens: HashMap<String, u32>,
    tens_suffixed: HashMap<String, (u32, &'static str)>,
    multipliers: HashMap<String, u32>, // the power of ten
    multipliers_suffixed: HashMap<String, (u32, &'static str)>,
    decimals: HashSet<String>,
    preceding_prefixers: HashMap<&'static str, char>,
    following_prefixers: HashMap<&'static str, char>,
    prefixes: HashSet<char>,
    specials: HashSet<&'static str>,
    words: HashSet<String>,
}

const ONES: [&str; 19] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
    "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
];
const TENS: [(&str, u32); 8] = [
    ("twenty", 20),
    ("thirty", 30),
    ("forty", 40),
    ("fifty", 50),
    ("sixty", 60),
    ("seventy", 70),
    ("eighty", 80),
    ("ninety", 90),
];
const MULTIPLIERS: [(&str, u32); 12] = [
    ("hundred", 2),
    ("thousand", 3),
    ("million", 6),
    ("billion", 9),
    ("trillion", 12),
    ("quadrillion", 15),
    ("quintillion", 18),
    ("sextillion", 21),
    ("septillion", 24),
    ("octillion", 27),
    ("nonillion", 30),
    ("decillion", 33),
];

static NUMBERS: LazyLock<Numbers> = LazyLock::new(|| {
    // Built with the same rules as the Python tables (so "nine" gives "nineth", as there).
    let ones: HashMap<String, u32> = ONES.iter().zip(1..).map(|(n, v)| (n.to_string(), v)).collect();
    let mut ones_suffixed = HashMap::new();
    for (name, &v) in &ones {
        let plural = if name == "six" { "sixes".to_string() } else { format!("{name}s") };
        ones_suffixed.insert(plural, (v, "s"));
    }
    for (k, v) in [("zeroth", (0, "th")), ("first", (1, "st")), ("second", (2, "nd")), ("third", (3, "rd")), ("fifth", (5, "th")), ("twelfth", (12, "th"))] {
        ones_suffixed.insert(k.to_string(), v);
    }
    for (name, &v) in &ones {
        if v > 3 && v != 5 && v != 12 {
            let tail = if name.ends_with('t') { "h" } else { "th" };
            ones_suffixed.insert(format!("{name}{tail}"), (v, "th"));
        }
    }
    let tens: HashMap<String, u32> = TENS.iter().map(|&(n, v)| (n.to_string(), v)).collect();
    let mut tens_suffixed = HashMap::new();
    for &(n, v) in &TENS {
        tens_suffixed.insert(n.replace('y', "ies"), (v, "s"));
    }
    for &(n, v) in &TENS {
        tens_suffixed.insert(n.replace('y', "ieth"), (v, "th"));
    }
    let multipliers: HashMap<String, u32> = MULTIPLIERS.iter().map(|&(n, k)| (n.to_string(), k)).collect();
    let mut multipliers_suffixed = HashMap::new();
    for &(n, k) in &MULTIPLIERS {
        multipliers_suffixed.insert(format!("{n}s"), (k, "s"));
    }
    for &(n, k) in &MULTIPLIERS {
        multipliers_suffixed.insert(format!("{n}th"), (k, "th"));
    }
    let zeros: HashSet<&str> = ["o", "oh", "zero"].into();
    let decimals: HashSet<String> =
        ones.keys().chain(tens.keys()).cloned().chain(zeros.iter().map(|z| z.to_string())).collect();
    let preceding_prefixers: HashMap<&str, char> =
        [("minus", '-'), ("negative", '-'), ("plus", '+'), ("positive", '+')].into();
    let following_prefixers: HashMap<&str, char> = [
        ("pound", '£'),
        ("pounds", '£'),
        ("euro", '€'),
        ("euros", '€'),
        ("dollar", '$'),
        ("dollars", '$'),
        ("cent", '¢'),
        ("cents", '¢'),
    ]
    .into();
    let prefixes = preceding_prefixers.values().chain(following_prefixers.values()).copied().collect();
    let specials: HashSet<&str> = ["and", "double", "triple", "point"].into();
    let mut words: HashSet<String> = HashSet::new();
    words.extend(zeros.iter().map(|s| s.to_string()));
    words.extend(ones.keys().cloned());
    words.extend(ones_suffixed.keys().cloned());
    words.extend(tens.keys().cloned());
    words.extend(tens_suffixed.keys().cloned());
    words.extend(multipliers.keys().cloned());
    words.extend(multipliers_suffixed.keys().cloned());
    words.extend(preceding_prefixers.keys().map(|s| s.to_string()));
    words.extend(following_prefixers.keys().map(|s| s.to_string()));
    words.extend(["per", "percent"].map(String::from));
    words.extend(specials.iter().map(|s| s.to_string()));
    Numbers {
        zeros,
        ones,
        ones_suffixed,
        tens,
        tens_suffixed,
        multipliers,
        multipliers_suffixed,
        decimals,
        preceding_prefixers,
        following_prefixers,
        prefixes,
        specials,
        words,
    }
});

/// `EnglishNumberNormalizer.process_words`.
#[allow(unused_assignments)] // `output!` resets `value` as Python's output() does, even when it is set again next
fn process_words(words: &[&str]) -> Vec<String> {
    let t = &*NUMBERS;
    let mut out = Vec::new();
    let mut prefix: Option<char> = None;
    let mut value: Option<Value> = None;
    let mut skip = false;

    // Python's `output()`: prefix the result, then reset value and prefix.
    macro_rules! output {
        ($r:expr) => {{
            let r: String = $r;
            let r = match prefix.take() {
                Some(p) => format!("{p}{r}"),
                None => r,
            };
            value = None;
            out.push(r);
        }};
    }
    let in_words = |w: Option<&str>| w.is_some_and(|w| t.words.contains(w));
    let is_numeric = |w: Option<&str>| w.is_some_and(|w| NUMERIC.is_match(w));

    for (i, &current) in words.iter().enumerate() {
        let prev = if i > 0 { Some(words[i - 1]) } else { None };
        let next = words.get(i + 1).copied();
        if skip {
            skip = false;
            continue;
        }
        let next_is_numeric = is_numeric(next);
        let first = current.chars().next().unwrap();
        let has_prefix = t.prefixes.contains(&first);
        let without_prefix = if has_prefix { &current[first.len_utf8()..] } else { current };
        let prev_in_ones = prev.is_some_and(|p| t.ones.contains_key(p));
        let prev_in_tens = prev.is_some_and(|p| t.tens.contains_key(p));

        if NUMERIC.is_match(without_prefix) {
            // arabic numbers (potentially with signs and fractions)
            let f = to_fraction(without_prefix).expect("numeric token");
            if let Some(v) = &value {
                if let Value::Str(s) = v
                    && s.ends_with('.')
                {
                    // concatenate decimals / ip address components
                    value = Some(Value::Str(format!("{s}{current}")));
                    continue;
                }
                output!(v.text());
            }
            if has_prefix {
                prefix = Some(first);
            }
            value = Some(match times_pow10(&f, 0) {
                Some(n) => Value::Int(n), // store integers as int
                None => Value::Str(without_prefix.to_string()),
            });
        } else if !t.words.contains(current) {
            // non-numeric words
            if let Some(v) = &value {
                output!(v.text());
            }
            output!(current.to_string());
        } else if t.zeros.contains(current) {
            value = Some(Value::Str(falsy_text(&value) + "0"));
        } else if let Some(&ones) = t.ones.get(current) {
            value = Some(match value.take() {
                None => Value::Int(ones.into()),
                Some(v) if matches!(v, Value::Str(_)) || prev_in_ones => {
                    let s = v.text();
                    if prev_in_tens && ones < 10 {
                        // replace the last zero with the digit
                        let mut c: Vec<char> = s.chars().collect();
                        c.pop();
                        Value::Str(format!("{}{ones}", c.into_iter().collect::<String>()))
                    } else {
                        Value::Str(format!("{s}{ones}"))
                    }
                }
                Some(Value::Int(n)) => {
                    let m = if ones < 10 { 10u32 } else { 100 };
                    if &n % m == BigUint::ZERO {
                        Value::Int(n + ones)
                    } else {
                        Value::Str(format!("{n}{ones}"))
                    }
                }
                Some(Value::Str(_)) => unreachable!(),
            });
        } else if let Some(&(ones, suffix)) = t.ones_suffixed.get(current) {
            // ordinal or cardinal; yield the number right away
            let r = match value.clone() {
                None => format!("{ones}{suffix}"),
                Some(v) if matches!(v, Value::Str(_)) || prev_in_ones => {
                    let s = v.text();
                    if prev_in_tens && ones < 10 {
                        let mut c: Vec<char> = s.chars().collect();
                        c.pop();
                        format!("{}{ones}{suffix}", c.into_iter().collect::<String>())
                    } else {
                        format!("{s}{ones}{suffix}")
                    }
                }
                Some(Value::Int(n)) => {
                    let m = if ones < 10 { 10u32 } else { 100 };
                    if &n % m == BigUint::ZERO {
                        format!("{}{suffix}", n + ones)
                    } else {
                        format!("{n}{ones}{suffix}")
                    }
                }
                Some(Value::Str(_)) => unreachable!(),
            };
            output!(r);
            value = None;
        } else if let Some(&tens) = t.tens.get(current) {
            value = Some(match value.take() {
                None => Value::Int(tens.into()),
                Some(Value::Str(s)) => Value::Str(format!("{s}{tens}")),
                Some(Value::Int(n)) => {
                    if &n % 100u32 == BigUint::ZERO {
                        Value::Int(n + tens)
                    } else {
                        Value::Str(format!("{n}{tens}"))
                    }
                }
            });
        } else if let Some(&(tens, suffix)) = t.tens_suffixed.get(current) {
            // ordinal or cardinal; yield the number right away
            let r = match value.clone() {
                None => format!("{tens}{suffix}"),
                Some(Value::Str(s)) => format!("{s}{tens}{suffix}"),
                Some(Value::Int(n)) => {
                    if &n % 100u32 == BigUint::ZERO {
                        format!("{}{suffix}", n + tens)
                    } else {
                        format!("{n}{tens}{suffix}")
                    }
                }
            };
            output!(r);
        } else if let Some(&k) = t.multipliers.get(current) {
            match value.clone() {
                None => value = Some(Value::Int(pow10(k))),
                Some(v) if matches!(&v, Value::Str(_)) || matches!(&v, Value::Int(n) if *n == BigUint::ZERO) => {
                    let p = match &v {
                        Value::Str(s) => to_fraction(s).and_then(|f| times_pow10(&f, k)),
                        Value::Int(n) => Some(n * pow10(k)),
                    };
                    match p {
                        Some(p) => value = Some(Value::Int(p)),
                        None => {
                            output!(v.text());
                            value = Some(Value::Int(pow10(k)));
                        }
                    }
                }
                Some(Value::Int(n)) => {
                    let before = &n / 1000u32 * 1000u32;
                    let residual = &n % 1000u32;
                    value = Some(Value::Int(before + residual * pow10(k)));
                }
                Some(Value::Str(_)) => unreachable!(),
            }
        } else if let Some(&(k, suffix)) = t.multipliers_suffixed.get(current) {
            match value.clone() {
                None => output!(format!("{}{suffix}", pow10(k))),
                Some(Value::Str(s)) => match to_fraction(&s).and_then(|f| times_pow10(&f, k)) {
                    Some(p) => output!(format!("{p}{suffix}")),
                    None => {
                        output!(s);
                        output!(format!("{}{suffix}", pow10(k)));
                    }
                },
                Some(Value::Int(n)) => {
                    let before = &n / 1000u32 * 1000u32;
                    let residual = &n % 1000u32;
                    let v = before + residual * pow10(k);
                    output!(format!("{v}{suffix}"));
                }
            }
            value = None;
        } else if let Some(&p) = t.preceding_prefixers.get(current) {
            // apply prefix (positive, minus, etc.) if it precedes a number
            if let Some(v) = &value {
                output!(v.text());
            }
            if in_words(next) || next_is_numeric {
                prefix = Some(p);
            } else {
                output!(current.to_string());
            }
        } else if let Some(&p) = t.following_prefixers.get(current) {
            // apply prefix (dollars, cents, etc.) only after a number
            if let Some(v) = &value {
                prefix = Some(p);
                output!(v.text());
            } else {
                output!(current.to_string());
            }
        } else if current == "per" || current == "percent" {
            // apply suffix symbols (percent -> '%')
            if let Some(v) = value.clone() {
                if current == "per" {
                    if next == Some("cent") {
                        output!(format!("{}%", v.text()));
                        skip = true;
                    } else {
                        output!(v.text());
                        output!(current.to_string());
                    }
                } else {
                    output!(format!("{}%", v.text()));
                }
            } else {
                output!(current.to_string());
            }
        } else if t.specials.contains(current) {
            if !in_words(next) && !next_is_numeric {
                // apply special handling only if the next word can be numeric
                if let Some(v) = &value {
                    output!(v.text());
                }
                output!(current.to_string());
            } else if current == "and" {
                // ignore "and" after hundreds, thousands, etc.
                if !prev.is_some_and(|p| t.multipliers.contains_key(p)) {
                    if let Some(v) = &value {
                        output!(v.text());
                    }
                    output!(current.to_string());
                }
            } else if current == "double" || current == "triple" {
                let n = next.unwrap_or("");
                if t.ones.contains_key(n) || t.zeros.contains(n) {
                    let repeats = if current == "double" { 2 } else { 3 };
                    let ones = t.ones.get(n).copied().unwrap_or(0);
                    value = Some(Value::Str(falsy_text(&value) + &ones.to_string().repeat(repeats)));
                    skip = true;
                } else {
                    if let Some(v) = &value {
                        output!(v.text());
                    }
                    output!(current.to_string());
                }
            } else if current == "point" && (next.is_some_and(|n| t.decimals.contains(n)) || next_is_numeric) {
                value = Some(Value::Str(falsy_text(&value) + "."));
            }
        } else {
            unreachable!("every number word is covered above: {current}");
        }
    }
    if let Some(v) = &value {
        output!(v.text());
    }
    out
}

/// `EnglishNumberNormalizer.preprocess`.
fn preprocess(s: &str) -> String {
    // Python's re.split(r"\band\s+a\s+half\b", s)
    let mut segments = Vec::new();
    let (mut copied, mut pos) = (0, 0);
    while let Some(m) = AND_A_HALF.find_at(s, pos) {
        let prev = s[..m.start()].chars().next_back();
        let next = s[m.end()..].chars().next();
        if !prev.is_some_and(py::is_word) && !next.is_some_and(py::is_word) {
            segments.push(&s[copied..m.start()]);
            copied = m.end();
            pos = m.end();
        } else {
            pos = m.start() + 1; // "and" is ASCII
        }
    }
    segments.push(&s[copied..]);

    let decimals = &NUMBERS.decimals;
    let mut results: Vec<&str> = Vec::new();
    let last = segments.len() - 1;
    for (i, segment) in segments.into_iter().enumerate() {
        if py::strip(segment).is_empty() {
            continue;
        }
        results.push(segment);
        if i != last {
            let last_word = *py::split(segment).last().unwrap();
            if decimals.contains(last_word) || NUMBERS.multipliers.contains_key(last_word) {
                results.push("point five");
            } else {
                results.push("and a half");
            }
        }
    }
    let s = results.join(" ");
    // put a space at number/letter boundary
    let s = LETTER_DIGIT.replace_all(&s, "${1} ${2}");
    let s = DIGIT_LETTER.replace_all(&s, "${1} ${2}");
    // but remove spaces which could be a suffix: ([0-9])\s+(st|nd|rd|th|s)\b
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    'scan: while i < c.len() {
        if c[i].is_ascii_digit() && c.get(i + 1).is_some_and(|&x| py::is_space(x)) {
            let mut j = i + 1;
            while j < c.len() && py::is_space(c[j]) {
                j += 1;
            }
            for suffix in ["st", "nd", "rd", "th", "s"] {
                let sc: Vec<char> = suffix.chars().collect();
                if c[j..].starts_with(&sc) && py::boundary(&c, j + sc.len()) {
                    out.push(c[i]);
                    out.push_str(suffix);
                    i = j + sc.len();
                    continue 'scan;
                }
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `EnglishNumberNormalizer.postprocess`.
fn postprocess(s: &str) -> String {
    // apply currency postprocessing; "$2 and ¢7" -> "$2.07"
    let s = sub_then_boundary(&COMBINE_CENTS, s, |m| {
        let cents: u32 = m[3].parse().unwrap();
        format!("{}{}.{cents:02}", &m[1], &m[2])
    });
    let s = sub_then_boundary(&EXTRACT_CENTS, &s, |m| format!("¢{}", m[1].parse::<u32>().unwrap()));
    // write "one(s)" instead of "1(s)", just for the readability
    sub_words(&s, &["1s", "1"], true, |m| if m == "1s" { "ones".into() } else { "one".into() })
}

fn standardize_numbers(s: &str) -> String {
    let s = preprocess(s);
    let s = process_words(&py::split(&s)).join(" ");
    postprocess(&s)
}

fn standardize_spellings(s: &str) -> String {
    let map = &*SPELLING;
    py::split(s).into_iter().map(|w| map.get(w).map_or(w, String::as_str)).collect::<Vec<_>>().join(" ")
}

/// (leading `\b`, literal, replacement); every pattern ends with `\b`. Order matters.
const REPLACERS: [(bool, &str, &str); 43] = [
    // common contractions
    (true, "won't", "will not"),
    (true, "can't", "can not"),
    (true, "let's", "let us"),
    (true, "ain't", "aint"),
    (true, "y'all", "you all"),
    (true, "wanna", "want to"),
    (true, "gotta", "got to"),
    (true, "gonna", "going to"),
    (true, "i'ma", "i am going to"),
    (true, "imma", "i am going to"),
    (true, "woulda", "would have"),
    (true, "coulda", "could have"),
    (true, "shoulda", "should have"),
    (true, "ma'am", "madam"),
    // contractions in titles/prefixes
    (true, "mr", "mister "),
    (true, "mrs", "missus "),
    (true, "st", "saint "),
    (true, "dr", "doctor "),
    (true, "prof", "professor "),
    (true, "capt", "captain "),
    (true, "gov", "governor "),
    (true, "ald", "alderman "),
    (true, "gen", "general "),
    (true, "sen", "senator "),
    (true, "rep", "representative "),
    (true, "pres", "president "),
    (true, "rev", "reverend "),
    (true, "hon", "honorable "),
    (true, "asst", "assistant "),
    (true, "assoc", "associate "),
    (true, "lt", "lieutenant "),
    (true, "col", "colonel "),
    (true, "jr", "junior "),
    (true, "sr", "senior "),
    (true, "esq", "esquire "),
    // perfect tenses
    (false, "'d been", " had been"),
    (false, "'s been", " has been"),
    (false, "'d gone", " had gone"),
    (false, "'s gone", " has gone"),
    (false, "'d done", " had done"),
    (false, "'s got", " has got"),
    // general contractions
    (false, "n't", " not"),
    (false, "'re", " are"),
];
const MORE_REPLACERS: [(&str, &str); 6] =
    [("'s", " is"), ("'d", " would"), ("'ll", " will"), ("'t", " not"), ("'ve", " have"), ("'m", " am")];

/// `ml.normalize_for_wer`: transformers' `WhisperTokenizer.normalize`.
pub fn normalize_for_wer(text: &str) -> String {
    let mut s = text.to_lowercase();
    s = BRACKETS.replace_all(&s, "").into_owned(); // remove words between brackets
    s = PARENS.replace_all(&s, "").into_owned(); // remove words between parenthesis
    s = sub_words(&s, &["hmm", "mm", "mhm", "mmm", "uh", "um"], true, |_| String::new());
    s = SPACE_APOSTROPHE.replace_all(&s, "'").into_owned();
    for (lead, lit, rep) in REPLACERS.iter().copied().chain(MORE_REPLACERS.iter().map(|&(l, r)| (false, l, r))) {
        s = sub_words(&s, &[lit], lead, |_| rep.to_string());
    }
    s = DIGIT_COMMA.replace_all(&s, "${1}${2}").into_owned(); // remove commas between digits
    s = PERIOD.replace_all(&s, " ${1}").into_owned(); // remove periods not followed by numbers
    s = remove_symbols_and_diacritics(&s, ".%$¢€£"); // keep some symbols for numerics
    s = standardize_numbers(&s);
    s = standardize_spellings(&s);
    // now remove prefix/suffix symbols that are not preceded/followed by numbers
    s = PREFIX_SYMBOL.replace_all(&s, " ${1}").into_owned();
    s = SUFFIX_SYMBOL.replace_all(&s, "${1} ").into_owned();
    SPACES.replace_all(&s, " ").into_owned()
}
