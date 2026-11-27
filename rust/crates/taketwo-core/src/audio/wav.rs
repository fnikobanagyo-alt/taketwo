//! audio.load and audio.save. Reading goes through `hound`; writing is a plain 44-byte PCM16 header and
//! samples converted as libsndfile 1.2 does for float data (soundfile.write, clipping on): the float times
//! 2^31, rounded to nearest-even (lrintf), shifted right by 16; at or above +1.0 it is 32767.

use super::resample;
use std::path::Path;

/// `audio.load(path, sr)`: mono float32 (channels averaged), resampled when `sr` differs from the file's rate.
pub fn load(path: &Path, sr: Option<u32>) -> Result<(Vec<f32>, u32), String> {
    let mut r = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = r.spec();
    let inter: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()
        }
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let ch = spec.channels.max(1) as usize;
    let mut a: Vec<f32> = if ch == 1 {
        inter
    } else {
        inter.chunks_exact(ch).map(super::np::mean_f32).collect()
    };
    let mut file_sr = spec.sample_rate;
    if let Some(sr) = sr
        && sr != 0
        && sr != file_sr
    {
        a = resample(&a, file_sr, sr);
        file_sr = sr;
    }
    Ok((a, file_sr))
}

fn pcm16(x: f32) -> i16 {
    let s = x * 2147483648.0f32;
    if s as f64 >= 2147483647.0 {
        32767
    } else if s as f64 <= -2147483648.0 {
        -32768
    } else {
        ((s.round_ties_even() as i64) >> 16) as i16
    }
}

/// The bytes `audio.save` writes: mono PCM16 WAV of `np.clip(audio, -1, 1)`.
pub fn pcm16_wav_bytes(audio: &[f32], sr: u32) -> Vec<u8> {
    let data_len = (audio.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + audio.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sr.to_le_bytes());
    out.extend_from_slice(&(sr * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &x in audio {
        out.extend_from_slice(&pcm16(x.clamp(-1.0, 1.0)).to_le_bytes());
    }
    out
}

/// `audio.save(path, audio, sr)`.
pub fn save(path: &Path, audio: &[f32], sr: u32) -> std::io::Result<()> {
    std::fs::write(path, pcm16_wav_bytes(audio, sr))
}
