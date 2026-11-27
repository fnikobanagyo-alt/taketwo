//! Word grouping ported from mlx-audio's whisper (`timing.py`: the tail of `find_alignment`,
//! `merge_punctuations`, `add_word_timestamps`; `whisper.py`: `HFTokenizerWrapper.split_to_word_tokens`).
//! Pure: tokens come as (id, raw bytes) and the DTW result as jump times, so it runs with no model.
//! Python's `a in b` on strings is substring containment (`"" in s` is true); ported that way.

use taketwo_core::text::py;

pub const PREPEND_PUNCTUATIONS: &str = "\"'\u{201c}\u{bf}([{-";
pub const APPEND_PUNCTUATIONS: &str = "\"'.\u{3002},\u{ff0c}!\u{ff01}?\u{ff1f}:\u{ff1a}\u{201d})]}\u{3001}";
const STRING_PUNCTUATION: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
const SENTENCE_END_MARKS: &str = ".\u{3002}!\u{ff01}?\u{ff1f}";
/// Languages `split_to_word_tokens` splits on unicode boundaries instead of spaces.
const NO_SPACE_LANGUAGES: [&str; 6] = ["zh", "ja", "th", "lo", "my", "yue"];

/// One token as the tokenizer knows it. `bytes` is the raw byte string of a text token; a special
/// token (id >= eot) decodes as `special_text`.
#[derive(Clone, Debug)]
pub struct Token {
    pub id: i32,
    pub bytes: Vec<u8>,
}

/// HF `decode` of a token list (skip_special_tokens=False): the bytes joined, invalid UTF-8 replaced.
fn decode(tokens: &[Token], eot: i32) -> String {
    let mut out = String::new();
    let mut buf = Vec::new();
    for t in tokens {
        if t.id >= eot {
            out.push_str(&String::from_utf8_lossy(&std::mem::take(&mut buf)));
            out.push_str(if t.id == eot { "<|endoftext|>" } else { "<|special|>" });
        } else {
            buf.extend_from_slice(&t.bytes);
        }
    }
    out.push_str(&String::from_utf8_lossy(&buf));
    out
}

fn split_tokens_on_unicode(tokens: &[Token], eot: i32) -> (Vec<String>, Vec<Vec<Token>>) {
    const REPLACEMENT: char = '\u{fffd}';
    let decoded_full: Vec<char> = decode(tokens, eot).chars().collect();
    let (mut words, mut word_tokens) = (Vec::new(), Vec::new());
    let mut current: Vec<Token> = Vec::new();
    let mut unicode_offset = 0usize;
    for t in tokens {
        current.push(t.clone());
        let decoded = decode(&current, eot);
        let chars: Vec<char> = decoded.chars().collect();
        let keep = match chars.iter().position(|&c| c == REPLACEMENT) {
            None => true,
            Some(i) => decoded_full.get(unicode_offset + i) == Some(&REPLACEMENT),
        };
        if keep {
            unicode_offset += chars.len();
            words.push(decoded);
            word_tokens.push(std::mem::take(&mut current));
        }
    }
    (words, word_tokens)
}

fn split_tokens_on_spaces(tokens: &[Token], eot: i32) -> (Vec<String>, Vec<Vec<Token>>) {
    let (subwords, subword_tokens) = split_tokens_on_unicode(tokens, eot);
    let (mut words, mut word_tokens): (Vec<String>, Vec<Vec<Token>>) = (Vec::new(), Vec::new());
    for (subword, toks) in subwords.into_iter().zip(subword_tokens) {
        let special = toks[0].id >= eot;
        let with_space = subword.starts_with(' ');
        let punctuation = STRING_PUNCTUATION.contains(py::strip(&subword));
        if special || with_space || punctuation || words.is_empty() {
            words.push(subword);
            word_tokens.push(toks);
        } else {
            words.last_mut().unwrap().push_str(&subword);
            word_tokens.last_mut().unwrap().extend(toks);
        }
    }
    (words, word_tokens)
}

/// `split_to_word_tokens` for the tokenizer's language.
pub fn split_to_word_tokens(tokens: &[Token], eot: i32, language: &str) -> (Vec<String>, Vec<Vec<Token>>) {
    if NO_SPACE_LANGUAGES.contains(&language) {
        split_tokens_on_unicode(tokens, eot)
    } else {
        split_tokens_on_spaces(tokens, eot)
    }
}

#[derive(Clone, Debug)]
pub struct WordTiming {
    pub word: String,
    pub n_tokens: usize,
    pub start: f64,
    pub end: f64,
    pub probability: f64,
}

/// The tail of `find_alignment` after the DTW: `text_tokens` are the window's text tokens (no eot),
/// `jump_times[k]` is when token k starts (len = tokens + 1; the last is when eot starts),
/// `token_probs[k]` is token k's probability.
pub fn alignment(text_tokens: &[Token], eot: i32, language: &str, jump_times: &[f64], token_probs: &[f64]) -> Vec<WordTiming> {
    if text_tokens.is_empty() {
        return Vec::new();
    }
    assert_eq!(jump_times.len(), text_tokens.len() + 1);
    assert_eq!(token_probs.len(), text_tokens.len());
    let mut all = text_tokens.to_vec();
    all.push(Token { id: eot, bytes: Vec::new() });
    let (words, word_tokens) = split_to_word_tokens(&all, eot, language);
    if word_tokens.len() <= 1 {
        return Vec::new();
    }
    let mut boundaries = vec![0usize];
    for t in &word_tokens[..word_tokens.len() - 1] {
        boundaries.push(boundaries.last().unwrap() + t.len());
    }
    let mut out = Vec::new();
    for (i, w) in boundaries.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        // np.mean of float32 probabilities
        let p = token_probs[a..b].iter().map(|&p| p as f32).sum::<f32>() / (b - a) as f32;
        out.push(WordTiming {
            word: words[i].clone(),
            n_tokens: word_tokens[i].len(),
            start: jump_times[a],
            end: jump_times[b],
            probability: p as f64,
        });
    }
    out
}

pub fn merge_punctuations(alignment: &mut [WordTiming], prepended: &str, appended: &str) {
    let n = alignment.len() as isize;
    let (mut i, mut j) = (n - 2, n - 1);
    while i >= 0 {
        let (iu, ju) = (i as usize, j as usize);
        if alignment[iu].word.starts_with(' ') && prepended.contains(py::strip(&alignment[iu].word)) {
            let prev = std::mem::take(&mut alignment[iu].word);
            alignment[ju].word = prev + &alignment[ju].word;
            alignment[ju].n_tokens += alignment[iu].n_tokens;
            alignment[iu].n_tokens = 0;
        } else {
            j = i;
        }
        i -= 1;
    }
    let (mut i, mut j) = (0usize, 1usize);
    while j < alignment.len() {
        if !alignment[i].word.ends_with(' ') && appended.contains(alignment[j].word.as_str()) {
            let following = std::mem::take(&mut alignment[j].word);
            alignment[i].word.push_str(&following);
            alignment[i].n_tokens += alignment[j].n_tokens;
            alignment[j].n_tokens = 0;
        } else {
            i = j;
        }
        j += 1;
    }
}

/// A segment as `add_word_timestamps` sees it: times in seconds, and how many text tokens it holds.
#[derive(Clone, Debug)]
pub struct SegmentTimes {
    pub start: f64,
    pub end: f64,
    pub n_text_tokens: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WordOut {
    pub word: String,
    pub start: f64,
    pub end: f64,
    pub probability: f64,
}

/// numpy's `round(x, 2)`: rint(x * 100) / 100.
fn round2(x: f64) -> f64 {
    (x * 100.0).round_ties_even() / 100.0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

/// `add_word_timestamps` for one decoding window, given its alignment (from [`alignment`]).
/// Updates the segments' start and end as Python does and returns each segment's words and the new
/// `last_speech_timestamp`. `time_offset` is the window start in seconds (0 when the alignment times
/// are already absolute).
pub fn add_word_timestamps(
    segments: &mut [SegmentTimes],
    mut alignment: Vec<WordTiming>,
    time_offset: f64,
    mut last_speech_timestamp: f64,
) -> (Vec<Vec<WordOut>>, f64) {
    if segments.is_empty() {
        return (Vec::new(), last_speech_timestamp);
    }
    let durations: Vec<f64> = alignment.iter().map(|t| t.end - t.start).filter(|&d| d != 0.0).collect();
    let median_duration = if durations.is_empty() { 0.0 } else { median(durations.clone()) };
    let median_duration = median_duration.min(0.7);
    let max_duration = median_duration * 2.0;

    if !durations.is_empty() {
        for i in 1..alignment.len() {
            if alignment[i].end - alignment[i].start > max_duration {
                if SENTENCE_END_MARKS.contains(alignment[i].word.as_str()) {
                    alignment[i].end = alignment[i].start + max_duration;
                } else if SENTENCE_END_MARKS.contains(alignment[i - 1].word.as_str()) {
                    alignment[i].start = alignment[i].end - max_duration;
                }
            }
        }
    }

    merge_punctuations(&mut alignment, PREPEND_PUNCTUATIONS, APPEND_PUNCTUATIONS);

    let mut word_index = 0usize;
    let mut out = Vec::new();
    for segment in segments.iter_mut() {
        let mut saved_tokens = 0usize;
        let mut words: Vec<WordOut> = Vec::new();
        while word_index < alignment.len() && saved_tokens < segment.n_text_tokens {
            let timing = &alignment[word_index];
            if !timing.word.is_empty() {
                words.push(WordOut {
                    word: timing.word.clone(),
                    start: round2(time_offset + timing.start),
                    end: round2(time_offset + timing.end),
                    probability: timing.probability,
                });
            }
            saved_tokens += timing.n_tokens;
            word_index += 1;
        }

        if !words.is_empty() {
            let n = words.len();
            if words[0].end - last_speech_timestamp > median_duration * 4.0
                && (words[0].end - words[0].start > max_duration
                    || (n > 1 && words[1].end - words[0].start > max_duration * 2.0))
            {
                if n > 1 && words[1].end - words[1].start > max_duration {
                    let boundary = (words[1].end / 2.0).max(words[1].end - max_duration);
                    words[0].end = boundary;
                    words[1].start = boundary;
                }
                words[0].start = (words[0].end - max_duration).max(0.0);
            }

            if segment.start < words[0].end && segment.start - 0.5 > words[0].start {
                words[0].start = (words[0].end - median_duration).min(segment.start).max(0.0);
            } else {
                segment.start = words[0].start;
            }

            let last = words.len() - 1;
            if segment.end > words[last].start && segment.end + 0.5 < words[last].end {
                words[last].end = (words[last].start + median_duration).max(segment.end);
            } else {
                segment.end = words[last].end;
            }
            last_speech_timestamp = segment.end;
        }
        out.push(words);
    }
    (out, last_speech_timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn f(v: &Value) -> f64 {
        v.as_f64().unwrap()
    }

    /// G1: mlx-audio's own grouping on canned tokens and jump times (rust/golden/record_whisper_words.py).
    #[test]
    fn whisper_word_grouping_matches_mlx_audio() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../golden/whisper_words.json");
        let golden: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cases = golden["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 16, "case count in whisper_words.json");
        for c in cases {
            let name = c["name"].as_str().unwrap();
            let eot = c["eot"].as_i64().unwrap() as i32;
            let tokens: Vec<Token> = c["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| Token {
                    id: t["id"].as_i64().unwrap() as i32,
                    bytes: t["bytes"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap() as u8).collect(),
                })
                .collect();
            let jumps: Vec<f64> = c["jump_times"].as_array().unwrap().iter().map(f).collect();
            let probs: Vec<f64> = c["token_probs"].as_array().unwrap().iter().map(f).collect();
            let mut segs: Vec<SegmentTimes> = c["segments_in"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| SegmentTimes { start: f(&s["start"]), end: f(&s["end"]), n_text_tokens: s["n_text_tokens"].as_u64().unwrap() as usize })
                .collect();
            let al = alignment(&tokens, eot, c["language"].as_str().unwrap(), &jumps, &probs);
            let (words, _) = add_word_timestamps(&mut segs, al, f(&c["time_offset"]), f(&c["last_speech_timestamp"]));
            let want = c["segments_out"].as_array().unwrap();
            assert_eq!(words.len(), want.len(), "{name}: segment count");
            for ((seg, ws), w) in segs.iter().zip(&words).zip(want) {
                assert!((seg.start - f(&w["start"])).abs() < 0.01, "{name}: segment start {} vs {}", seg.start, w["start"]);
                assert!((seg.end - f(&w["end"])).abs() < 0.01, "{name}: segment end {} vs {}", seg.end, w["end"]);
                let ww = w["words"].as_array().unwrap();
                let got: Vec<&str> = ws.iter().map(|x| x.word.as_str()).collect();
                let exp: Vec<&str> = ww.iter().map(|x| x["word"].as_str().unwrap()).collect();
                assert_eq!(got, exp, "{name}: words");
                for (g, e) in ws.iter().zip(ww) {
                    assert!((g.start - f(&e["start"])).abs() < 0.01, "{name}: {:?} start {} vs {}", g.word, g.start, e["start"]);
                    assert!((g.end - f(&e["end"])).abs() < 0.01, "{name}: {:?} end {} vs {}", g.word, g.end, e["end"]);
                    assert!((g.probability - f(&e["probability"])).abs() < 1e-5, "{name}: {:?} p {} vs {}", g.word, g.probability, e["probability"]);
                }
            }
        }
    }
}
