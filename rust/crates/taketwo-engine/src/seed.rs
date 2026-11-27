//! `taketwo-engine seed --fixture contract/fixture.json`: the Rust engine's contract seeder.
//! Writes what contract/seed_python.py `seed()` writes, into the isolated library only.

use serde_json::Value;
use std::path::{Path, PathBuf};
use taketwo_core::library::{Library, is_plain_name};
use taketwo_core::paths::Mode;

const SR: u32 = 24000;

/// seed_python.py `tone()`: 24 kHz mono 16-bit, amplitude 8000.
pub fn tone(path: &Path, seconds: f64, hz: f64) -> Result<(), String> {
    let spec = hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| format!("{}: {e}", path.display()))?;
    let n = ((seconds * SR as f64) as i64).max(1);
    for i in 0..n {
        let s = (8000.0 * (2.0 * std::f64::consts::PI * hz * i as f64 / SR as f64).sin()) as i16; // int() truncates
        w.write_sample(s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

fn name<'a>(v: &'a Value, what: &str) -> Result<&'a str, String> {
    v.as_str().filter(|s| is_plain_name(s)).ok_or_else(|| format!("fixture: bad {what} {v}"))
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

pub fn seed(data: &Path, fixture: &Path) -> Result<(), String> {
    seed_into(&Library::open(data)?, fixture)
}

/// `seed` into a library that is already open (tests pin its clock first).
pub fn seed_into(lib: &Library, fixture: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(fixture).map_err(|e| format!("{}: {e}", fixture.display()))?;
    let fx: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", fixture.display()))?;
    for p in fx["profiles"].as_array().into_iter().flatten() {
        let dir = lib.profiles_dir().join(name(&p["id"], "profile id")?);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        tone(&dir.join("preview.wav"), 1.0, 220.0)?;
        lib.put_profile(&mut p.as_object().cloned().ok_or("fixture: profile is not an object")?)?;
    }
    for g in fx["generations"].as_array().into_iter().flatten() {
        let dir = lib.generations_dir().join(name(&g["id"], "narration id")?);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for c in g["chunks"].as_array().into_iter().flatten() {
            if truthy(c.get("file")) {
                let i = c["i"].as_f64().ok_or("fixture: chunk without i")?;
                tone(&dir.join(name(&c["file"], "chunk file")?), 0.5, 200.0 + 20.0 * i)?;
            }
        }
        if truthy(g.get("file")) {
            tone(&dir.join("final.wav"), g["duration"].as_f64().ok_or("fixture: narration without duration")?, 220.0)?;
        }
        lib.put_generation(&mut g.as_object().cloned().ok_or("fixture: narration is not an object")?)?;
    }
    Ok(())
}

/// `seed [--fixture <file>]`; refuses anything but isolated mode, before it writes anything.
pub fn run(args: &[String]) -> Result<(), String> {
    let data = match Mode::from_env_and_args(args)? {
        Mode::Isolated { data } => data,
        Mode::Live => return Err("refusing to seed the live library; set TAKETWO_DATA to a temporary folder".into()),
    };
    let mut fixture = PathBuf::from("contract/fixture.json");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--fixture" => fixture = it.next().ok_or("--fixture needs a file")?.into(),
            other => return Err(format!("seed: unknown argument {other}")),
        }
    }
    seed(&data, &fixture)?;
    println!("seeded {} from {}", data.display(), fixture.display());
    Ok(())
}
