//! Qwen3-TTS 1.7B Base voice cloning through the vendored Candle crate (step S12): `engines.synthesize` and its
//! `synthesis_report()` for the `qwen3` family (backend/app/engines.py lines 311-397, qwen_compat.py).
//! Candle and MLX draw different random numbers, so a take never matches Python's take for the same seed;
//! only the settings, the token limit, the lengths and the report are ported exactly.

use candle_core::{DType, Device, Tensor};
use qwen_tts::model::Model;
use qwen_tts::model::loader::{LoaderConfig, ModelLoader};
use qwen_tts::model::options::VoiceCloneOptions;
use qwen_tts::model::types::DecodeLengths;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use taketwo_core::library::Doc;
use taketwo_core::render::models::{Take, Voice};
use taketwo_core::text::py;

/// The error a cancelled take returns, so the caller can tell it from a failure (the whisper binding's word too).
pub const CANCELLED: &str = qwen_tts::nn::generation::CANCELLED;

/// The model folder under `TAKETWO_MODELS`.
pub const MODEL_DIR: &str = "qwen3-tts-1.7b-base";

/// The engine ids this voice answers to (engines.py `qwen3-tts-1.7b`).
pub const ENGINES: [&str; 1] = ["qwen3-tts-1.7b"];

/// The `qwen3` family's parameter defaults (engines.py `_QWEN_PARAMS`).
/// shortcut: taketwo_core::delivery does not define these (only taketwo-engine's api.rs lists them for /api/engines),
/// so this is the voice's one copy; move both to taketwo-core when a step may edit it.
pub const DEFAULTS: [(&str, f64); 4] = [("temperature", 0.7), ("top_p", 0.9), ("top_k", 40.0), ("repetition_penalty", 1.05)];

/// engines.py `LANG_NAMES`.
pub const LANG_NAMES: [(&str, &str); 10] = [
    ("en", "english"), ("zh", "chinese"), ("ja", "japanese"), ("ko", "korean"), ("de", "german"),
    ("fr", "french"), ("es", "spanish"), ("it", "italian"), ("pt", "portuguese"), ("ru", "russian"),
];

/// `LANG_NAMES.get(language, "auto")`.
pub fn lang_name(code: &str) -> &'static str {
    LANG_NAMES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n).unwrap_or("auto")
}

/// `int(12.5 * (expected_seconds * 1.8 + 3.0)) + 8`: truncated before the `+ 8`, as Python's `int()` does.
pub fn max_tokens(expected_seconds: f64) -> i64 {
    let max_sec = expected_seconds * 1.8 + 3.0;
    (12.5 * max_sec) as i64 + 8
}

/// The four settings a take uses, typed as Python's `float()` / `int()` leave them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampler {
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: i64,
    pub repetition_penalty: f64,
}

/// The family defaults, then the caller's non-null `params` on top.
pub fn sampler(params: &Doc) -> Result<Sampler, String> {
    let mut p = Doc::new();
    for (k, v) in DEFAULTS {
        p.insert(k.into(), json!(v));
    }
    for (k, v) in params {
        if !v.is_null() {
            p.insert(k.clone(), v.clone());
        }
    }
    Ok(Sampler {
        temperature: py::num(&p["temperature"])?,
        top_p: py::num(&p["top_p"])?,
        top_k: py::int(&p["top_k"])?,
        repetition_penalty: py::num(&p["repetition_penalty"])?,
    })
}

/// The crate options for one take. The sub-talker (code predictor) gets the caller's temperature, top-p and top-k
/// on every call, so the crate's defaults (0.9 / 50 / 1.0) never apply; it has no repetition penalty
/// (`SamplingConfig::for_subtalker` sets 1.0), so the penalty only touches codebook 0.
pub fn clone_options(s: &Sampler, max_new_tokens: usize) -> VoiceCloneOptions {
    let top_k = s.top_k.max(0) as usize;
    VoiceCloneOptions {
        do_sample: Some(true),
        temperature: Some(s.temperature),
        top_p: Some(s.top_p),
        top_k: Some(top_k),
        repetition_penalty: Some(s.repetition_penalty),
        subtalker_do_sample: Some(true),
        subtalker_temperature: Some(s.temperature),
        subtalker_top_p: Some(s.top_p),
        subtalker_top_k: Some(top_k),
        max_new_tokens: Some(max_new_tokens),
        non_streaming_mode: None,
    }
}

/// `_last_synthesis` for one qwen3 take, keys in Python's order.
pub fn report(tokens: usize, cap: i64, raw_samples: usize, sr: u32, s: &Sampler, decoder: Option<DecodeLengths>) -> Doc {
    let stop = if cap == 0 {
        "not_reported"
    } else if tokens as i64 >= cap {
        "length_limit_reached"
    } else {
        "ended_before_limit"
    };
    let decoder = match decoder {
        Some(d) => json!({"frames": d.frames, "samples": d.samples, "zero_codes": d.zero_codes}),
        None => json!({}),
    };
    match json!({
        "tokens": tokens, "token_limit": cap, "raw_samples": raw_samples, "sample_rate": sr, "stop_reason": stop,
        "sampler": {"temperature": s.temperature, "top_p": s.top_p, "top_k": s.top_k, "repetition_penalty": s.repetition_penalty},
        "generation_route": "icl_explicit_penalty",
        "decoder": decoder,
    }) {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

/// Metal with bf16 under `metal`, CUDA (bf16) under `cuda`, else the CPU with f32.
fn device() -> Result<(Device, DType), String> {
    #[cfg(feature = "metal")]
    {
        return Ok((Device::new_metal(0).map_err(|e| e.to_string())?, DType::BF16));
    }
    #[cfg(all(feature = "cuda", not(feature = "metal")))]
    {
        return Ok((Device::new_cuda(0).map_err(|e| e.to_string())?, DType::BF16));
    }
    #[allow(unreachable_code)]
    Ok((Device::Cpu, DType::F32))
}

/// The Qwen voice: loaded on first use (or by `load`), kept between takes, dropped by `unload`.
pub struct QwenVoice {
    dir: PathBuf,
    model: Option<Model>,
}

impl QwenVoice {
    pub fn new(dir: impl Into<PathBuf>) -> QwenVoice {
        QwenVoice { dir: dir.into(), model: None }
    }

    /// `$TAKETWO_MODELS/qwen3-tts-1.7b-base`.
    pub fn from_env() -> Result<QwenVoice, String> {
        let root = std::env::var_os("TAKETWO_MODELS").ok_or("TAKETWO_MODELS is not set")?;
        Ok(QwenVoice::new(Path::new(&root).join(MODEL_DIR)))
    }

    pub fn is_loaded(&self) -> bool {
        self.model.is_some()
    }

    pub fn load(&mut self) -> Result<(), String> {
        if self.model.is_some() {
            return Ok(());
        }
        let (device, dtype) = device()?;
        let cfg = LoaderConfig { dtype, use_flash_attn: cfg!(feature = "flash-attn"), ..LoaderConfig::default() };
        let loader = ModelLoader::from_local_dir(&self.dir).map_err(|e| format!("Qwen model at {}: {e}", self.dir.display()))?;
        let model = loader.load_tts_model(&device, &cfg).map_err(|e| format!("Qwen model load: {e}"))?;
        if !model.is_base_model() || !model.has_text_processor() {
            return Err(format!("{} is not a Qwen3-TTS Base model with its text tokenizer", self.dir.display()));
        }
        self.model = Some(model);
        Ok(())
    }

    /// The loaded crate model, for measurements.
    pub fn model(&self) -> Option<&Model> {
        self.model.as_ref()
    }

    /// Frees the model (the idle timer that calls this is S16's).
    pub fn unload(&mut self) {
        self.model = None;
    }

    #[allow(clippy::too_many_arguments)]
    fn take(&mut self, text: &str, ref_wav: &Path, ref_text: &str, language: &str, s: &Sampler, seed: u32, cap: i64, cancel: &AtomicBool) -> Result<Take, String> {
        self.load()?;
        let model = self.model.as_mut().expect("loaded");
        // every random draw of this take (talker and code predictor) comes from this one global MT19937
        qwen_tts::nn::mt_rng::set_seed(seed as u64);
        let sr = model.sample_rate() as u32;
        let (ref_audio, _) = taketwo_core::audio::wav::load(ref_wav, Some(sr))?;
        let ref_t = Tensor::from_vec(ref_audio.clone(), (ref_audio.len(),), model.device()).map_err(|e| e.to_string())?;
        let prompt = model.create_voice_clone_prompt_from_audio(&ref_t, Some(ref_text.to_string()), false).map_err(|e| e.to_string())?;
        // the crate falls back to x-vector only with a warning; VoiceForge always takes the ICL route
        if !prompt.icl_mode {
            return Err("Qwen could not encode the reference clip for in-context cloning.".into());
        }
        let ids = model.tokenize_text(text)?;
        let text_ids = Tensor::from_vec(ids.clone(), (1, ids.len()), model.device()).map_err(|e| e.to_string())?;
        let opts = clone_options(s, cap.max(0) as usize);
        let res = model.generate_voice_clone_full_cancellable(&text_ids, &prompt, lang_name(language), Some(opts), cancel).map_err(|e| e.to_string())?;
        let audio: Vec<f32> = res.audio.to_dtype(DType::F32).and_then(|a| a.flatten_all()).and_then(|a| a.to_vec1()).map_err(|e| e.to_string())?;
        let upsample = model.decode_upsample_rate().map_err(|e| e.to_string())?;
        if audio.len() % upsample != 0 {
            return Err(format!("Qwen take has {} samples, not a multiple of {upsample}.", audio.len()));
        }
        let tokens = res.effective_lengths.as_ref().and_then(|l| l.first().copied()).unwrap_or(0);
        let report = report(tokens, cap, audio.len(), res.sample_rate as u32, s, res.decoder);
        Ok(Take { audio, sr: res.sample_rate as u32, report })
    }
}

impl Voice for QwenVoice {
    fn synthesize(
        &mut self,
        engine: &str,
        text: &str,
        ref_wav: &Path,
        ref_text: &str,
        language: &str,
        params: &Doc,
        seed: u32,
        expected_seconds: f64,
        cancel: &AtomicBool,
    ) -> Result<Take, String> {
        if !ENGINES.contains(&engine) {
            return Err(format!("Engine {engine} is not in TakeTwo 1.0."));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let s = sampler(params)?;
        let cap = max_tokens(expected_seconds);
        self.take(text, ref_wav, ref_text, language, &s, seed, cap, cancel).map_err(|e| if cancel.load(Ordering::Relaxed) { CANCELLED.into() } else { e })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwen_tts::model::config::GenerateConfig;
    use qwen_tts::nn::sampling::sample_token;

    fn doc(v: Value) -> Doc {
        match v {
            Value::Object(m) => m,
            _ => panic!(),
        }
    }

    #[test]
    fn qwen_max_tokens_matches_python() {
        // int(12.5 * (e * 1.8 + 3.0)) + 8 printed by python3; 8.2 and 16.2 land a hair under an integer
        let cases = [
            (0.0, 45), (0.5, 56), (1.0, 68), (1.2, 72), (2.0, 90), (2.2, 95), (2.5, 101), (3.3, 119), (4.444, 145),
            (6.0, 180), (7.77, 220), (8.2, 229), (10.0, 270), (12.345, 323), (16.2, 409), (0.1, 47), (33.0, 788),
            (1.0 / 3.0, 53),
        ];
        for (e, want) in cases {
            assert_eq!(max_tokens(e), want, "{e}");
        }
    }

    #[test]
    fn qwen_sampler_defaults_then_caller_params() {
        assert_eq!(sampler(&Doc::new()).unwrap(), Sampler { temperature: 0.7, top_p: 0.9, top_k: 40, repetition_penalty: 1.05 });
        let s = sampler(&doc(json!({"temperature": 0.55, "top_p": null, "top_k": 25.9, "seed": 3}))).unwrap();
        assert_eq!(s, Sampler { temperature: 0.55, top_p: 0.9, top_k: 25, repetition_penalty: 1.05 });
        assert!(sampler(&doc(json!({"temperature": "hot"}))).is_err());
    }

    #[test]
    fn qwen_language_names() {
        assert_eq!(lang_name("en"), "english");
        assert_eq!(lang_name("ru"), "russian");
        assert_eq!(lang_name("xx"), "auto");
        assert_eq!(lang_name(""), "auto");
        for (_, name) in LANG_NAMES {
            assert!(qwen_tts::model::types::SUPPORTED_LANGUAGES.contains(&name), "{name}");
        }
    }

    /// Listening rule 1: the caller's settings reach the code predictor on the voice-clone path
    /// (`merge_voice_clone` is what `generate_voice_clone_full_cancellable` calls; `for_subtalker` is what
    /// `ConditionalGeneration::generate_cancellable` hands to the code predictor), whatever the model's
    /// generation_config.json says.
    #[test]
    fn qwen_subtalker_takes_the_callers_settings() {
        let model_file = r#"{"do_sample": true, "repetition_penalty": 1.05, "temperature": 0.9, "top_p": 1.0, "top_k": 50,
            "subtalker_dosample": true, "subtalker_temperature": 0.9, "subtalker_top_p": 1.0, "subtalker_top_k": 50, "max_new_tokens": 8192}"#;
        for defaults in [GenerateConfig::default(), GenerateConfig::from_json(model_file).unwrap()] {
            for s in [sampler(&Doc::new()).unwrap(), Sampler { temperature: 0.55, top_p: 0.8, top_k: 25, repetition_penalty: 1.2 }] {
                let talker = defaults.merge_voice_clone(&clone_options(&s, 77));
                assert_eq!((talker.temperature, talker.top_p, talker.top_k, talker.repetition_penalty), (s.temperature, s.top_p, s.top_k as usize, s.repetition_penalty));
                assert!(talker.do_sample);
                let sub = talker.for_subtalker();
                assert_eq!((sub.temperature, sub.top_p, sub.top_k), (s.temperature, s.top_p, s.top_k as usize));
                assert!(sub.do_sample);
                assert_eq!(sub.repetition_penalty, 1.0);
            }
        }
    }

    /// The 1.05 penalty acts on codebook 0 (the talker) and not on the code predictor, greedy so it is exact.
    #[test]
    fn qwen_repetition_penalty_on_codebook_0_only() {
        let s = sampler(&Doc::new()).unwrap();
        let mut talker = GenerateConfig::default().merge_voice_clone(&clone_options(&s, 10));
        talker.do_sample = false;
        let mut sub = talker.for_subtalker();
        sub.do_sample = false;
        // token 2 leads by less than the penalty takes off it, once it has been said
        let logits = Tensor::new(&[1.0f32, 0.5, 1.04, 0.0], &Device::Cpu).unwrap();
        assert_eq!(sample_token(&logits, &talker, &[]).unwrap(), 2);
        assert_eq!(sample_token(&logits, &talker, &[2]).unwrap(), 0);
        assert_eq!(sample_token(&logits, &sub, &[2]).unwrap(), 2);
    }

    #[test]
    fn qwen_report_keys_in_python_order() {
        let s = sampler(&Doc::new()).unwrap();
        let d = DecodeLengths { frames: 200, samples: 200 * 1920, zero_codes: 1 };
        let r = report(30, 68, 30 * 1920, 24000, &s, Some(d));
        let keys: Vec<&str> = r.keys().map(String::as_str).collect();
        assert_eq!(keys, ["tokens", "token_limit", "raw_samples", "sample_rate", "stop_reason", "sampler", "generation_route", "decoder"]);
        assert_eq!(r["stop_reason"], "ended_before_limit");
        assert_eq!(serde_json::to_string(&r["sampler"]).unwrap(), r#"{"temperature":0.7,"top_p":0.9,"top_k":40,"repetition_penalty":1.05}"#);
        assert_eq!(serde_json::to_string(&r["decoder"]).unwrap(), r#"{"frames":200,"samples":384000,"zero_codes":1}"#);
        assert_eq!(r["generation_route"], "icl_explicit_penalty");
        assert_eq!(report(68, 68, 0, 24000, &s, None)["stop_reason"], "length_limit_reached");
        assert_eq!(report(69, 68, 0, 24000, &s, None)["stop_reason"], "length_limit_reached");
        assert_eq!(serde_json::to_string(&report(0, 68, 0, 24000, &s, None)["decoder"]).unwrap(), "{}");
    }

    #[test]
    fn qwen_cancelled_before_start_and_unknown_engine() {
        let mut v = QwenVoice::new("/nonexistent/qwen");
        let p = Path::new("/nonexistent/ref.wav");
        let stop = AtomicBool::new(true);
        assert_eq!(v.synthesize("qwen3-tts-1.7b", "Hi.", p, "Hi.", "en", &Doc::new(), 1, 1.0, &stop).unwrap_err(), CANCELLED);
        let go = AtomicBool::new(false);
        assert!(v.synthesize("chatterbox", "Hi.", p, "Hi.", "en", &Doc::new(), 1, 1.0, &go).unwrap_err().contains("not in TakeTwo"));
        // a missing model is a failure, not a cancel
        assert_ne!(v.synthesize("qwen3-tts-1.7b", "Hi.", p, "Hi.", "en", &Doc::new(), 1, 1.0, &go).unwrap_err(), CANCELLED);
        assert!(!v.is_loaded());
    }
}
