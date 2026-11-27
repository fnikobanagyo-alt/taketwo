//! Step S16: a whole narration (`taketwo_core::render::narration`) with the scripted fake: assembly, speed,
//! final check, run_generation, cancel, delete while running, re-speak, takes and fix-remaining.
//! Takes come from rust/golden/render (no new recorder); ffmpeg is needed for the WAV/MP3 export and is
//! reported (not silently skipped) where it is missing.

mod render_common;

use render_common::{bytes, f32s, golden, obj};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use taketwo_core::audio::{self, ffmpeg};
use taketwo_core::library::{Doc, Library};
use taketwo_core::render::models::fake::{Fake, Kind};
use taketwo_core::render::models::Take;
use taketwo_core::render::narration::{self, Ctx, Models, STOPPED};
use taketwo_core::render::RenderError;
use taketwo_core::transcript::Transcript;

fn have_ffmpeg(test: &str) -> bool {
    let ok = std::process::Command::new(ffmpeg::program()).arg("-version").output().is_ok_and(|o| o.status.success());
    if !ok {
        println!("{test}: ffmpeg is not on PATH (or TAKETWO_FFMPEG); the WAV/MP3 export cannot run, so this test checks nothing here");
    }
    ok
}

/// Take `n` of a golden render case, with its transcript and embedding.
fn scripted(case: &str, n: usize) -> (Take, Transcript, Vec<f32>) {
    let g = golden();
    let c = g["render"].as_array().unwrap().iter().find(|c| c["name"] == case).unwrap();
    let sc = &c["script"];
    let t = &sc["takes"][n];
    let take = Take { audio: f32s(&t["audio"]), sr: t["sr"].as_u64().unwrap() as u32, report: obj(&t["report"]) };
    let tr: Transcript = serde_json::from_value(sc["transcripts"][n].clone()).unwrap();
    let e = sc["embeddings"][n].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
    (take, tr, e)
}

fn sine(sr: u32, secs: f64, natural: bool) -> Vec<f32> {
    let n = (sr as f64 * secs) as usize;
    let mut a: Vec<f32> = (0..n).map(|i| (0.4 * (2.0 * std::f64::consts::PI * 200.0 * i as f64 / sr as f64).sin()) as f32).collect();
    if natural {
        // np.geomspace(1.0, 1e-5, 4800) over the last 0.2 s
        let m = 4800.min(n);
        for k in 0..m {
            a[n - m + k] *= 10f64.powf(-5.0 * k as f64 / (m - 1) as f64) as f32;
        }
    }
    a
}

/// A temporary library with the golden voice `rvoice` (and a mic profile, so backfill runs no ffmpeg).
struct World {
    _tmp: render_common::TempDir,
    lib: Library,
    phases: Rc<RefCell<Vec<String>>>,
}

impl World {
    fn new(tag: &str) -> World {
        let tmp = render_common::tempdir(tag);
        let lib = Library::open(&tmp.0.join("library")).unwrap();
        let g = golden();
        let c = g["render"].as_array().unwrap().iter().find(|c| c["name"] == "clean_first").unwrap();
        let pdir = lib.profiles_dir().join("rvoice");
        std::fs::create_dir_all(&pdir).unwrap();
        for (name, f) in c["voice"].as_object().unwrap() {
            std::fs::write(pdir.join(name), bytes(f)).unwrap();
        }
        let (a, sr) = audio::load(&pdir.join("ref_0.wav"), Some(24000)).unwrap();
        let bands: Vec<String> = taketwo_core::delivery::ltas(&a, sr).iter().map(|b| if b.is_nan() { "NaN".into() } else { format!("{b:?}") }).collect();
        std::fs::write(pdir.join("mic.json"), format!("{{\"sr\": 24000, \"bands\": [{}]}}", bands.join(", "))).unwrap();
        let mut p = obj(&c["inputs"]["profile"]);
        lib.put_profile(&mut p).unwrap();
        World { _tmp: tmp, lib, phases: Rc::new(RefCell::new(Vec::new())) }
    }

    fn gdir(&self, gid: &str) -> PathBuf {
        self.lib.generations_dir().join(gid)
    }

    /// A narration with `n` parts of the golden text; `settings` on top of the golden case's.
    fn narration(&self, gid: &str, n: usize, settings: Value) -> Doc {
        let g = golden();
        let c = g["render"].as_array().unwrap().iter().find(|c| c["name"] == "clean_first").unwrap();
        let mut s = obj(&c["inputs"]["settings"]);
        s.extend(obj(&settings));
        let chunks: Vec<Value> = (0..n)
            .map(|i| json!({"i": i, "text": "The river ran past the old mill.", "pause_after": if i + 1 == n { "end" } else { "sentence" }, "status": "pending"}))
            .collect();
        let mut d = obj(&json!({"id": gid, "profile_id": "rvoice", "profile_name": "Render voice", "engine": "qwen3-tts-1.7b", "language": "en",
            "params": {}, "speed": 1.0, "favorite": false, "title": "River", "text": "x", "cleaned_text": "x", "status": "queued", "chunks": chunks}));
        d.insert("settings".into(), Value::Object(s));
        std::fs::create_dir_all(self.gdir(gid)).unwrap();
        self.lib.put_generation(&mut d).unwrap();
        d
    }

    fn get(&self, gid: &str) -> Doc {
        self.lib.get_generation(gid).unwrap().unwrap()
    }

    /// Run `f` with the context and models over `fake`. `on_clock` runs on every clock reading.
    fn run<T>(&self, fake: &Fake, on_clock: &dyn Fn(usize), f: impl FnOnce(&mut Ctx, &mut Models) -> T) -> T {
        let flag = fake.flag();
        let (mut v, mut t, mut e, c) = (fake.clone(), fake.clone(), fake.clone(), fake.clone());
        let mut n = 0usize;
        let mut clock = || {
            on_clock(n);
            n += 1;
            c.now()
        };
        let phases = self.phases.clone();
        let phase = move |name: &str, f: &mut dyn FnMut()| {
            phases.borrow_mut().push(name.to_string());
            f()
        };
        let mut nonce = || 7u32;
        let mut load = || Ok(());
        let mut cache = HashMap::new();
        let mut cx = Ctx { lib: &self.lib, clock: &mut clock, phase: &phase };
        let mut m = Models { voice: &mut v, transcriber: &mut t, encoder: &mut e, cancel: &flag, nonce: &mut nonce, load: &mut load, pitch_cache: &mut cache };
        f(&mut cx, &mut m)
    }
}

fn fake(takes: &[(&str, usize)]) -> Fake {
    let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
    for (case, n) in takes {
        let (t, tr, e) = scripted(case, *n);
        a.push(t);
        b.push(tr);
        c.push(e);
    }
    Fake::new(a, b, c, 1_000_000.0, 1.3)
}

fn nothing(_: usize) {}

fn s_doc() -> Doc {
    obj(&json!({"sentence_pause_ms": 300, "paragraph_pause_ms": 700, "target_lufs": -16.0}))
}

fn put_part(w: &World, gid: &str, i: usize, a: &[f32], sr: u32, pause_after: &str) -> Value {
    let name = format!("chunk_{i:03}.wav");
    audio::save(&w.gdir(gid).join(&name), a, sr).unwrap();
    json!({"i": i, "text": "x", "pause_after": pause_after, "status": "ok", "issues": [], "file": name, "sr": sr})
}

fn bare(gid: &str, chunks: Vec<Value>, extra: Value) -> Doc {
    let mut d = obj(&json!({"id": gid, "profile_id": "rvoice", "voice_settings": {"mic_match": false, "studio_finish": false}, "chunks": chunks}));
    d.extend(obj(&extra));
    d
}

#[test]
fn assemble_timeline_pauses_and_files() {
    if !have_ffmpeg("assemble_timeline_pauses_and_files") {
        return;
    }
    let w = World::new("asm");
    std::fs::create_dir_all(w.gdir("ga")).unwrap();
    let sr = 24000;
    let (a, b, c) = (sine(sr, 1.0, false), sine(sr, 0.5, false), sine(sr, 0.25, false));
    assert_eq!(audio::edge_silence(&a, sr), (0.0, 0.0), "a plain tone has no silent edges, so the pauses are the settings");
    let chunks = vec![put_part(&w, "ga", 0, &a, sr, "paragraph"), json!({"i": 1, "text": "skipped", "pause_after": "sentence", "status": "pending"}),
                      put_part(&w, "ga", 2, &b, sr, "sentence"), put_part(&w, "ga", 3, &c, sr, "sentence")];
    let mut g = bare("ga", chunks, json!({"mic_eq": [1.0]}));
    w.run(&fake(&[]), &nothing, |cx, _| narration::assemble(cx, &mut g, &s_doc())).unwrap();
    // lead 0.15; part 0 (1 s) + 700 ms; part 2 (0.5 s) + 300 ms; part 3 (0.25 s), its pause dropped as the last
    assert_eq!(g["timeline"], json!([{"i": 0, "start": 0.15, "end": 1.15}, {"i": 2, "start": 1.85, "end": 2.35}, {"i": 3, "start": 2.65, "end": 2.9}]));
    let master = audio::load(&w.gdir("ga").join("master.wav"), None).unwrap().0;
    assert_eq!(master.len(), 3600 + 24000 + 16800 + 12000 + 7200 + 6000 + 9600);
    assert_eq!(g["duration"], json!(3.3)); // 79200 / 24000
    assert_eq!((g["file"].as_str(), g["sample_rate"].as_u64()), (Some("final.wav"), Some(24000)));
    assert_eq!(g["mic_eq"], Value::Null, "mic_eq is null when mic match is off");
    assert!(g["audio_rev"].as_f64().is_some());
    let fin = audio::load(&w.gdir("ga").join("final.wav"), None).unwrap().0;
    assert_eq!(fin, master, "1.0x: final.wav is the master");
    assert!(std::fs::metadata(w.gdir("ga").join("final.mp3")).unwrap().len() > 0);
    for tmp in ["master.tmp.wav", "final.tmp.wav", "final.tmp.mp3"] {
        assert!(!w.gdir("ga").join(tmp).exists(), "{tmp}");
    }
    assert_eq!(*w.phases.borrow(), ["assembly", "speed_export"]);
}

/// test_regressions.py `test_assembly_preserves_part_duration_and_wav_export`.
#[test]
fn assembly_preserves_part_duration_and_wav_export() {
    if !have_ffmpeg("assembly_preserves_part_duration_and_wav_export") {
        return;
    }
    let w = World::new("asm1");
    std::fs::create_dir_all(w.gdir("g1")).unwrap();
    let sr = 24000;
    let mut a = sine(sr, 1.0, true);
    a.extend(std::iter::repeat_n(0.0, 3600));
    let mut g = bare("g1", vec![put_part(&w, "g1", 0, &a, sr, "end")], json!({}));
    w.run(&fake(&[]), &nothing, |cx, _| narration::assemble(cx, &mut g, &s_doc())).unwrap();
    let (master, sm) = audio::load(&w.gdir("g1").join("master.wav"), None).unwrap();
    let (fin, sf) = audio::load(&w.gdir("g1").join("final.wav"), None).unwrap();
    assert_eq!((fin == master, sm, sf), (true, sr, sr));
    assert_eq!(master.len(), a.len() + (0.55 * sr as f64) as usize);
    let t = &g["timeline"][0];
    assert!((t["end"].as_f64().unwrap() - t["start"].as_f64().unwrap() - a.len() as f64 / sr as f64).abs() < 5e-4);
    assert!(std::fs::metadata(w.gdir("g1").join("final.mp3")).unwrap().len() > 0);
}

/// test_audit_fixes.py `test_failed_speed_export_keeps_wav_and_mp3_together`, on both paths that publish.
#[test]
fn a_failed_export_leaves_the_old_pair_and_no_temp_files() {
    let w = World::new("fail");
    let gdir = w.gdir("gf");
    std::fs::create_dir_all(&gdir).unwrap();
    std::fs::write(gdir.join("final.wav"), b"old wav").unwrap();
    std::fs::write(gdir.join("final.mp3"), b"old mp3").unwrap();
    let e = narration::apply_speed(&gdir, 1.0, Some(&gdir.join("missing.wav")), &taketwo_core::render::no_phase).unwrap_err();
    assert!(e.message().contains("missing.wav"), "{e:?}");
    let check = |why: &str| {
        assert_eq!(std::fs::read(gdir.join("final.wav")).unwrap(), b"old wav", "{why}");
        assert_eq!(std::fs::read(gdir.join("final.mp3")).unwrap(), b"old mp3", "{why}");
        for tmp in ["final.tmp.wav", "final.tmp.mp3", "master.tmp.wav", "master.wav"] {
            assert!(!gdir.join(tmp).exists(), "{why}: {tmp}");
        }
    };
    check("copy failed");
    if !have_ffmpeg("a_failed_export_leaves_the_old_pair_and_no_temp_files (atempo half)") {
        return;
    }
    // atempo refuses 0.1: assemble's master.tmp.wav is removed, the old pair and no master stay
    let mut g = bare("gf", vec![put_part(&w, "gf", 0, &sine(24000, 0.5, true), 24000, "end")], json!({"speed": 0.1}));
    let e = w.run(&fake(&[]), &nothing, |cx, _| narration::assemble(cx, &mut g, &s_doc())).unwrap_err();
    assert!(e.message().contains("failed"), "{e:?}");
    check("atempo failed");
    assert!(g.get("timeline").is_none(), "nothing recorded on the narration");
}

#[test]
fn speed_change_retimes_the_export() {
    let w = World::new("speed");
    let gdir = w.gdir("gs");
    std::fs::create_dir_all(&gdir).unwrap();
    let mut g = bare("gs", vec![], json!({"speed": 1.0, "duration": 2.0, "timeline": [{"i": 0, "start": 0.15, "end": 1.15}]}));
    let mut clock = || 5.0;
    let e = narration::retime(&w.lib.generations_dir(), &mut g, 1.25, &mut clock, &taketwo_core::render::no_phase).unwrap_err();
    assert_eq!(e, RenderError::Value("Re-render this generation to change its speed.".into()));
    if !have_ffmpeg("speed_change_retimes_the_export") {
        return;
    }
    let master = sine(24000, 2.0, true);
    audio::save(&gdir.join("master.wav"), &master, 24000).unwrap();
    narration::retime(&w.lib.generations_dir(), &mut g, 1.25, &mut clock, &taketwo_core::render::no_phase).unwrap();
    assert_eq!(g["timeline"], json!([{"i": 0, "start": 0.12, "end": 0.92}]));
    assert_eq!((g["duration"].as_f64(), g["speed"].as_f64(), g["audio_rev"].as_f64()), (Some(1.6), Some(1.25), Some(5.0)));
    let fin = audio::load(&gdir.join("final.wav"), None).unwrap().0;
    let want = master.len() as f64 / 1.25;
    assert!((fin.len() as f64 - want).abs() < 0.01 * want, "atempo length {} against {want}", fin.len());
    assert!(std::fs::metadata(gdir.join("final.mp3")).unwrap().len() > 0);
    // back to 1.0x: a copy of the master
    narration::retime(&w.lib.generations_dir(), &mut g, 1.0, &mut clock, &taketwo_core::render::no_phase).unwrap();
    assert_eq!(audio::load(&gdir.join("final.wav"), None).unwrap().0.len(), master.len());
    assert_eq!(g["timeline"], json!([{"i": 0, "start": 0.15, "end": 1.15}]));
}

/// test_regressions.py `test_final_scan_checks_raw_cutoff_despite_fade_and_padding` and
/// `test_final_scan_preserves_legacy_known_failures`, plus the missing-export branch.
#[test]
fn final_scan_branches() {
    let w = World::new("scan");
    let gdir = w.gdir("gc");
    std::fs::create_dir_all(&gdir).unwrap();
    let sr = 24000;
    let ph = &taketwo_core::render::no_phase;
    // raw cut-off hidden by a fade and padding in the final file
    let a = sine(sr, 1.0, false);
    let mut fin = vec![0.0f32; 3600];
    fin.extend(audio::fade(&a, sr, 0, 100));
    fin.extend(std::iter::repeat_n(0.0, 12000));
    taketwo_core::render::files::write_float_wav(&gdir.join("raw.wav"), &a, sr).unwrap();
    audio::save(&gdir.join("final.wav"), &fin, sr).unwrap();
    let e = audio::end_analysis(&a, sr, 0.95, 0.12);
    let ending = json!({"start": e.start, "cut": e.cut, "natural": e.natural, "junk": e.junk, "last_level": e.last_level});
    let mut g = bare("gc", vec![json!({"i": 0, "text": "hello", "raw_file": "raw.wav", "issues": [], "ending": ending})],
                     json!({"timeline": [{"i": 0, "start": 0.15, "end": 1.65}]}));
    assert_eq!(narration::final_scan(&w.lib, &mut g, ph).unwrap(), vec![0]);
    assert!(g["chunks"][0]["final_issues"].as_array().unwrap().contains(&json!("cut_off")));
    // a legacy known failure stays failed though the audio decays naturally
    let n = sine(sr, 1.0, true);
    let mut padded = n.clone();
    padded.extend(std::iter::repeat_n(0.0, sr as usize));
    audio::save(&gdir.join("final.wav"), &padded, sr).unwrap();
    let mut g = bare("gc", vec![json!({"i": 0, "text": "hello", "issues": ["cut_off"]})], json!({"timeline": [{"i": 0, "start": 0, "end": 2}]}));
    assert_eq!(narration::final_scan(&w.lib, &mut g, ph).unwrap(), vec![0]);
    assert_eq!(g["chunks"][0]["final_issues"], json!(["cut_off"]));
    // a saved part matching the final mix passes; without the final file it is missing from the mix
    audio::save(&gdir.join("chunk_000.wav"), &n, sr).unwrap();
    let mut g = bare("gc", vec![json!({"i": 0, "text": "hello", "issues": [], "file": "chunk_000.wav"})], json!({"timeline": [{"i": 0, "start": 0, "end": 1}]}));
    assert_eq!(narration::final_scan(&w.lib, &mut g, ph).unwrap(), Vec::<i64>::new());
    assert_eq!(g["chunks"][0]["final_issues"], json!([]));
    std::fs::remove_file(gdir.join("final.wav")).unwrap();
    assert_eq!(narration::final_scan(&w.lib, &mut g, ph).unwrap(), vec![0]);
    assert_eq!(g["chunks"][0]["final_issues"], json!(["processed_audio_missing"]));
    // speech lost from the mix at the part's edge
    let mut lost = padded.clone();
    for x in &mut lost[18000..24000] {
        *x = 0.0;
    }
    audio::save(&gdir.join("final.wav"), &lost, sr).unwrap();
    let mut g = bare("gc", vec![json!({"i": 0, "text": "hello", "issues": [], "file": "chunk_000.wav"})], json!({"timeline": [{"i": 0, "start": 0, "end": 1}]}));
    assert_eq!(narration::final_scan(&w.lib, &mut g, ph).unwrap(), vec![0]);
    assert_eq!(g["chunks"][0]["final_issues"], json!(["processed_audio_cut_off"]));
}

#[test]
fn a_full_render_records_assembles_checks_and_summarises() {
    if !have_ffmpeg("a_full_render_records_assembles_checks_and_summarises") {
        return;
    }
    let w = World::new("run");
    w.narration("gr", 2, json!({}));
    let fk = fake(&[("clean_first", 0), ("clean_first", 0)]);
    w.run(&fk, &nothing, |cx, m| narration::run_generation(cx, m, "gr", None)).unwrap();
    let g = w.get("gr");
    assert_eq!(g["status"], "done", "{}", g["message"]);
    assert_eq!(fk.left(), (0, 0, 0), "one take per part");
    for c in g["chunks"].as_array().unwrap() {
        assert_eq!((c["status"].as_str(), c["final_issues"].clone()), (Some("ok"), json!([])), "{c}");
    }
    assert_eq!(g["final_scan"], json!({"checked": 2, "attempted": [], "fixed": [], "remaining": []}));
    assert_eq!((g["retries"].as_i64(), g["progress"].as_f64(), g["eta"].as_i64()), (Some(0), Some(1.0), Some(0)));
    assert_eq!(g["message"], "Done. All parts passed the automated checks.");
    assert!(g["render_seconds"].as_f64().unwrap() > 0.0);
    assert_eq!(g["timeline"].as_array().unwrap().len(), 2);
    assert!(g["mic_eq"].is_array(), "mic match is on by default and the voice has mic.json");
    for f in ["chunk_000.wav", "chunk_001.wav", "raw_chunk_000.wav", "master.wav", "final.wav", "final.mp3"] {
        assert!(w.gdir("gr").join(f).exists(), "{f}");
    }
    let p = w.lib.get_profile("rvoice").unwrap().unwrap();
    assert!(p["variants"][0].get("expressiveness").is_some(), "backfill tagged the voice");
    let ph = w.phases.borrow();
    for name in ["synthesis", "transcription", "quality_check", "assembly", "speed_export", "final_check"] {
        assert!(ph.iter().any(|p| p == name), "{name} in {ph:?}");
    }
}

#[test]
fn a_stop_during_a_take_is_saved_as_cancelled() {
    let w = World::new("cancel");
    w.narration("gx", 2, json!({}));
    let fk = fake(&[("clean_first", 0), ("clean_first", 0)]);
    fk.cancel_after(Kind::Synthesize, 1);
    w.run(&fk, &nothing, |cx, m| narration::run_generation(cx, m, "gx", None)).unwrap();
    let g = w.get("gx");
    assert_eq!((g["status"].as_str(), g["message"].as_str()), (Some("cancelled"), Some(STOPPED)));
    assert_eq!(g["chunks"][0]["status"], "pending", "the running part goes back to pending");
    assert!(!w.gdir("gx").join("final.wav").exists());
}

/// A narration removed while it renders stops the run and is not recreated; a soft delete stays
/// (test_review_loop.py `test_a_render_saving_after_delete_keeps_it_deleted`).
#[test]
fn a_narration_deleted_while_running_stays_deleted() {
    let w = World::new("del");
    w.narration("gd", 1, json!({}));
    let fk = fake(&[("clean_first", 0)]);
    let lib = &w.lib;
    w.run(&fk, &|n| if n == 0 { lib.delete_generation("gd").unwrap() }, |cx, m| narration::run_generation(cx, m, "gd", None)).unwrap();
    assert!(w.lib.get_generation("gd").unwrap().is_none(), "never recreated");
    let mut g = w.narration("gs", 1, json!({}));
    w.lib.set_generation_deleted("gs", true).unwrap();
    assert!(w.lib.put_generation_live(&mut g).unwrap());
    assert!(w.get("gs").contains_key("deleted_at"));
}

/// test_regressions.py `test_single_part_replacement_and_final_mix_are_rechecked` (a cut-off take kept, then
/// re-recorded clean after the final check) and `test_failed_replacement_remains_flagged_and_is_not_reported_fixed`.
#[test]
fn final_check_re_records_and_reports_fixed_or_remaining() {
    if !have_ffmpeg("final_check_re_records_and_reports_fixed_or_remaining") {
        return;
    }
    let w = World::new("recheck");
    w.narration("gk", 1, json!({"max_attempts": 1}));
    let fk = fake(&[("cutoff_then_clean", 0), ("cutoff_then_clean", 1)]);
    w.run(&fk, &nothing, |cx, m| narration::run_generation(cx, m, "gk", Some(&[0]))).unwrap();
    let g = w.get("gk");
    assert_eq!(fk.count(Kind::Synthesize), 2, "the final check re-recorded the part once");
    assert_eq!(g["final_scan"], json!({"checked": 1, "attempted": [0], "fixed": [0], "remaining": []}));
    assert_eq!((g["chunks"][0]["final_fix"].as_bool(), g["chunks"][0]["regen_count"].as_i64()), (Some(true), Some(1)));
    assert_eq!(g["status"], "done");

    w.narration("gm", 1, json!({"max_attempts": 1}));
    let fk = fake(&[("cutoff_then_clean", 0), ("cutoff_then_clean", 0)]);
    w.run(&fk, &nothing, |cx, m| narration::run_generation(cx, m, "gm", Some(&[0]))).unwrap();
    let g = w.get("gm");
    assert_eq!(g["final_scan"], json!({"checked": 1, "attempted": [0], "fixed": [], "remaining": [0]}));
    assert_eq!((g["flagged"].clone(), g["chunks"][0]["final_fix"].as_bool()), (json!([0]), Some(false)));
    assert!(g["chunks"][0]["final_issues"].as_array().unwrap().contains(&json!("cut_off")), "kept apart from the take's own issues");
    assert!(g["message"].as_str().unwrap().starts_with("Done, needs review: part 1."));
}

/// A finished one-part narration with take 1 (the `clean_first` take).
fn rendered(w: &World, gid: &str) {
    w.narration(gid, 1, json!({"max_attempts": 1}));
    w.run(&fake(&[("clean_first", 0)]), &nothing, |cx, m| narration::run_generation(cx, m, gid, None)).unwrap();
    assert_eq!(w.get(gid)["status"], "done");
}

/// test_review_loop.py `test_respeak_keeps_take_1_playable_next_to_take_2`.
#[test]
fn respeak_keeps_take_1_next_to_take_2() {
    if !have_ffmpeg("respeak_keeps_take_1_next_to_take_2") {
        return;
    }
    let w = World::new("respeak");
    rendered(&w, "gt");
    let take1 = std::fs::read(w.gdir("gt").join("chunk_000.wav")).unwrap();
    // take 2 is cut off, so the final check records it once more (and keeps it flagged)
    w.run(&fake(&[("cutoff_then_clean", 0); 2]), &nothing, |cx, m| narration::regenerate_chunk(cx, m, "gt", 0, Some("The river ran past the  old mill."))).unwrap();
    let g = w.get("gt");
    let ch = &g["chunks"][0];
    assert_eq!((ch["take"].as_i64(), ch["takes"].as_array().unwrap().len(), ch["takes"][0]["take"].as_i64()), (Some(2), 1, Some(1)));
    assert_eq!(ch["text"], "The river ran past the old mill.", "cleaned replacement text");
    let gens = w.lib.generations_dir();
    let p1 = taketwo_core::review::take_audio(&gens, &g, 0, 1).unwrap().unwrap();
    assert_eq!(std::fs::read(p1).unwrap(), take1, "take 1 is still playable");
    assert_ne!(std::fs::read(taketwo_core::review::take_audio(&gens, &g, 0, 2).unwrap().unwrap()).unwrap(), take1);
    for f in std::fs::read_dir(w.gdir("gt")).unwrap() {
        assert!(!f.unwrap().file_name().to_string_lossy().ends_with(".bak"), "snapshots discarded");
    }
}

/// test_audit_fixes.py `test_failed_respeak_restores_text_and_audio`, test_review_loop.py
/// `test_a_failed_respeak_leaves_no_orphan_take`, and the stopped re-speak (`test_stopped_respeak_restores_text_and_audio`).
#[test]
fn a_failed_or_stopped_respeak_restores_text_audio_and_leaves_no_orphan_take() {
    if !have_ffmpeg("a_failed_or_stopped_respeak_restores_text_audio_and_leaves_no_orphan_take") {
        return;
    }
    let w = World::new("respeak-fail");
    rendered(&w, "gf");
    let before = w.get("gf");
    let files: Vec<Vec<u8>> = ["chunk_000.wav", "final.wav", "final.mp3", "master.wav"].iter().map(|f| std::fs::read(w.gdir("gf").join(f)).unwrap()).collect();
    let same = |why: &str| {
        let g = w.get("gf");
        assert_eq!(g["chunks"][0]["text"], before["chunks"][0]["text"], "{why}");
        assert!(g["chunks"][0].get("takes").is_none(), "{why}: no take recorded");
        assert!(!w.gdir("gf").join("takes/000-1").exists(), "{why}: no orphan take folder");
        for (f, b) in ["chunk_000.wav", "final.wav", "final.mp3", "master.wav"].iter().zip(&files) {
            assert_eq!(&std::fs::read(w.gdir("gf").join(f)).unwrap(), b, "{why}: {f}");
        }
        g
    };
    // no take left in the script: the run fails
    let e = w.run(&fake(&[]), &nothing, |cx, m| narration::regenerate_chunk(cx, m, "gf", 0, Some("A brand new line."))).unwrap_err();
    assert!(e.message().contains("no take left"), "{e:?}");
    let g = same("failed");
    assert_eq!(g["message"], "The re-speak did not finish. The previous text and audio were kept.");
    // stopped after its synthesis
    let fk = fake(&[("clean_first", 0)]);
    fk.cancel_after(Kind::Synthesize, 1);
    w.run(&fk, &nothing, |cx, m| narration::regenerate_chunk(cx, m, "gf", 0, Some("A brand new line."))).unwrap();
    let g = same("stopped");
    assert_eq!(g["message"], "The re-speak was stopped. The previous text and audio were kept.");
    assert_eq!(g["status"], "done");
}

/// test_review_loop.py `test_keeping_take_1_swaps_audio_and_measurements_and_remixes`,
/// `test_switching_takes_clears_sounds_fine`, `test_unknown_take_is_refused`,
/// `test_a_take_switch_on_a_missing_or_deleted_narration_is_refused`, `test_a_delete_during_a_take_switch_stays_deleted`.
#[test]
fn select_take_swaps_back_remixes_and_refuses() {
    if !have_ffmpeg("select_take_swaps_back_remixes_and_refuses") {
        return;
    }
    let w = World::new("take");
    rendered(&w, "gt");
    let take1 = std::fs::read(w.gdir("gt").join("chunk_000.wav")).unwrap();
    let heard1 = w.get("gt")["chunks"][0]["heard"].clone();
    w.run(&fake(&[("cutoff_then_clean", 0); 2]), &nothing, |cx, m| narration::regenerate_chunk(cx, m, "gt", 0, None)).unwrap();
    let take2 = std::fs::read(w.gdir("gt").join("chunk_000.wav")).unwrap();
    let mut g = w.get("gt");
    g["chunks"][0]["accepted"] = json!({"by": "person", "at": 1});
    let rev = g["chunks"][0]["audio_revision"].as_i64().unwrap();
    let old_rev = g["audio_rev"].clone();
    w.lib.put_generation(&mut g).unwrap();
    for (gid, k, want) in [("gt", 9, "That take is no longer kept."), ("nosuch", 1, "This narration was deleted.")] {
        let e = w.run(&fake(&[]), &nothing, |cx, _| narration::select_take(cx, gid, 0, k)).unwrap_err();
        assert_eq!(e, RenderError::Value(want.into()));
    }
    w.run(&fake(&[]), &nothing, |cx, _| narration::select_take(cx, "gt", 0, 1)).unwrap();
    let g = w.get("gt");
    let ch = &g["chunks"][0];
    assert_eq!((ch["take"].as_i64(), ch["heard"].clone(), ch["accepted"].clone()), (Some(1), heard1, Value::Null));
    assert_eq!(ch["audio_revision"].as_i64(), Some(rev + 1));
    assert_eq!(std::fs::read(w.gdir("gt").join("chunk_000.wav")).unwrap(), take1);
    assert_eq!(ch["takes"].as_array().unwrap().iter().map(|t| t["take"].as_i64().unwrap()).collect::<Vec<_>>(), [2], "take 2 can be chosen again");
    assert_eq!(std::fs::read(taketwo_core::review::take_audio(&w.lib.generations_dir(), &g, 0, 2).unwrap().unwrap()).unwrap(), take2);
    assert_ne!(g["audio_rev"], old_rev, "re-mixed");
    // as Python: a take switch re-mixes but runs no final check, so the re-speak's `final_scan.remaining` still counts
    assert_eq!(taketwo_core::review::outcome(&g).unwrap(), Some("needs_listen"));
    // a delete made while the switch re-mixes stays
    let lib = &w.lib;
    w.run(&fake(&[]), &|n| if n == 1 { lib.set_generation_deleted("gt", true).unwrap(); }, |cx, _| narration::select_take(cx, "gt", 0, 2)).unwrap();
    assert!(w.get("gt").contains_key("deleted_at"));
    let e = w.run(&fake(&[]), &nothing, |cx, _| narration::select_take(cx, "gt", 0, 1)).unwrap_err();
    assert_eq!(e, RenderError::Value("This narration was deleted.".into()));
}

/// test_audit_fixes.py `test_fix_remaining_targets_every_part_needing_attention` / `..._corrects_the_stored_script`:
/// a flagged part and a part with a web artifact are re-recorded, the script corrected; nothing left is refused.
#[test]
fn fix_remaining_rerecords_targets_and_corrects_the_script() {
    if !have_ffmpeg("fix_remaining_rerecords_targets_and_corrects_the_script") {
        return;
    }
    let w = World::new("fix");
    w.narration("gz", 3, json!({"max_attempts": 1})); // one take per part: the corrected text no longer matches the scripted transcript
    w.run(&fake(&[("clean_first", 0); 3]), &nothing, |cx, m| narration::run_generation(cx, m, "gz", None)).unwrap();
    let mut g = w.get("gz");
    g["chunks"][1]["status"] = "flagged".into();
    g["chunks"][1]["issues"] = json!(["cut_off"]);
    g["chunks"][2]["text"] = "Tom&#39;s mill.".into();
    g["text"] = "Tom&#39;s mill.".into();
    w.lib.put_generation(&mut g).unwrap();
    let fk = fake(&[("clean_first", 0), ("clean_first", 0)]);
    w.run(&fk, &nothing, |cx, m| narration::fix_remaining(cx, m, "gz")).unwrap();
    let g = w.get("gz");
    assert_eq!(fk.count(Kind::Synthesize), 2, "parts 2 and 3 only");
    assert_eq!((g["text"].as_str(), g["chunks"][2]["text"].as_str()), (Some("Tom's mill."), Some("Tom's mill.")));
    assert_eq!(g["chunks"].as_array().unwrap().iter().map(|c| c["regen_count"].as_i64()).collect::<Vec<_>>(), [None, Some(1), Some(1)]);
    assert_eq!(g["status"], "done");
    rendered(&w, "gc");
    let e = w.run(&fake(&[]), &nothing, |cx, m| narration::fix_remaining(cx, m, "gc")).unwrap_err();
    assert_eq!(e, RenderError::Value("Nothing left to fix.".into()));
}

#[test]
fn backfill_tags_the_voice_once_and_keeps_an_existing_mic_profile() {
    let w = World::new("backfill");
    let pdir = w.lib.profiles_dir().join("rvoice");
    let mic = std::fs::read(pdir.join("mic.json")).unwrap();
    let mut p = w.lib.get_profile("rvoice").unwrap().unwrap();
    assert!(narration::backfill(&mut p, &pdir).unwrap());
    let v = &p["variants"][0];
    assert_eq!((v["expressiveness"].as_f64(), v["delivery"].as_str()), (Some(0.5), Some("calm")), "one clip");
    assert!(v["pitch_range"].as_f64().is_some());
    assert!(p["pitch_hz"].is_null() || p["pitch_hz"].as_f64().is_some());
    assert_eq!(std::fs::read(pdir.join("mic.json")).unwrap(), mic);
    let again = p.clone();
    let changed = narration::backfill(&mut p, &pdir).unwrap();
    assert_eq!(p, again);
    assert!(!changed, "a tagged voice is left alone");
}

#[test]
fn a_deleted_voice_or_unknown_engine_is_a_clear_error() {
    let w = World::new("errors");
    let mut g = w.narration("ge", 1, json!({}));
    g["engine"] = "voxcpm2".into();
    w.lib.put_generation(&mut g).unwrap();
    let e = w.run(&fake(&[]), &nothing, |cx, m| narration::run_generation(cx, m, "ge", None)).unwrap_err();
    assert!(e.message().contains("not in TakeTwo 1.0"), "{e:?}");
    w.lib.delete_profile("rvoice").unwrap();
    let e = w.run(&fake(&[]), &nothing, |cx, m| narration::run_generation(cx, m, "ge", None)).unwrap_err();
    assert_eq!(e, RenderError::Value("Voice profile was deleted.".into()));
}
