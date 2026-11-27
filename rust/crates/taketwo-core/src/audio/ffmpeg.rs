//! audio.decode_to_wav and audio.export_mp3: ffmpeg run as a separate program with Python's exact
//! arguments. The program is `TAKETWO_FFMPEG` when set, else `ffmpeg` on PATH. A missing or failing
//! program is an error value. `atempo` is pipeline.apply_speed's tempo change.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

pub fn program() -> OsString {
    std::env::var_os("TAKETWO_FFMPEG").filter(|p| !p.is_empty()).unwrap_or_else(|| "ffmpeg".into())
}

fn os(v: &[&str]) -> Vec<OsString> {
    v.iter().map(OsString::from).collect()
}

/// Python's argument list for decode_to_wav. `if max_seconds:` skips None and 0; the number is written as
/// Python's `str(float)` ("300.0", "2.5"), which Rust's `{:?}` matches for ordinary values.
pub fn decode_args(src: &Path, dst: &Path, sr: u32, max_seconds: Option<f64>) -> Vec<OsString> {
    let mut cmd = os(&["-y", "-hide_banner", "-loglevel", "error", "-i"]);
    cmd.push(src.into());
    cmd.extend(os(&["-vn", "-ac", "1", "-ar"]));
    cmd.push(sr.to_string().into());
    if let Some(t) = max_seconds.filter(|t| *t != 0.0) {
        cmd.extend(os(&["-t", &format!("{t:?}")]));
    }
    cmd.extend(os(&["-c:a", "pcm_f32le"]));
    cmd.push(dst.into());
    cmd
}

pub fn export_args(wav: &Path, mp3: &Path) -> Vec<OsString> {
    let mut cmd = os(&["-y", "-hide_banner", "-loglevel", "error", "-i"]);
    cmd.push(wav.into());
    cmd.extend(os(&["-b:a", "192k"]));
    cmd.push(mp3.into());
    cmd
}

fn run(args: Vec<OsString>) -> Result<(), String> {
    run_with(program(), args)
}

fn run_with(prog: OsString, args: Vec<OsString>) -> Result<(), String> {
    let status = Command::new(&prog)
        .args(&args)
        .status()
        .map_err(|e| format!("could not run {}: {e} (set TAKETWO_FFMPEG or put ffmpeg on PATH)", prog.to_string_lossy()))?;
    if status.success() { Ok(()) } else { Err(format!("{} failed: {status}", prog.to_string_lossy())) }
}

/// Decode any format ffmpeg understands to a mono float32 WAV at `sr`.
pub fn decode_to_wav(src: &Path, dst: &Path, sr: u32, max_seconds: Option<f64>) -> Result<(), String> {
    run(decode_args(src, dst, sr, max_seconds))
}

pub fn export_mp3(wav: &Path, mp3: &Path) -> Result<(), String> {
    run(export_args(wav, mp3))
}

/// pipeline.apply_speed's arguments: a pitch-preserving tempo change to 16-bit PCM (`atempo={speed:.3f}`).
pub fn atempo_args(src: &Path, dst: &Path, speed: f64) -> Vec<OsString> {
    let mut cmd = os(&["-y", "-hide_banner", "-loglevel", "error", "-i"]);
    cmd.push(src.into());
    cmd.extend(os(&["-af", &format!("atempo={speed:.3}"), "-c:a", "pcm_s16le"]));
    cmd.push(dst.into());
    cmd
}

pub fn atempo(src: &Path, dst: &Path, speed: f64) -> Result<(), String> {
    run(atempo_args(src, dst, speed))
}

#[cfg(test)]
mod tests {
    #[test]
    fn audio_ffmpeg_missing_program_is_an_error() {
        let e = super::run_with("/nonexistent/ffmpeg".into(), vec!["-version".into()]).unwrap_err();
        assert!(e.contains("could not run /nonexistent/ffmpeg"), "{e}");
    }
}
