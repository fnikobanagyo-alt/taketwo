//! The three file formats render_chunk touches beyond audio.save: a voice's `fingerprint.npy`, the 32-bit float
//! WAVs (`soundfile.write(..., subtype="FLOAT")`) and `save_json`. No new crate: `hound` writes float WAVs.

use std::path::Path;

/// `np.load(path)` for a 1-D little-endian float32 or float64 array (what `ml.speaker_embedding` saves).
/// float64 is narrowed to float32. shortcut: numpy would compute the cosine in float64 then; voices store float32.
pub fn read_npy(path: &Path) -> Result<Vec<f32>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bad = |m: &str| format!("{}: {m}", path.display());
    if b.len() < 10 || &b[..6] != b"\x93NUMPY" {
        return Err(bad("not a .npy file"));
    }
    let (hlen, start) = match b[6] {
        1 => (u16::from_le_bytes([b[8], b[9]]) as usize, 10),
        2 | 3 if b.len() >= 12 => (u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize, 12),
        _ => return Err(bad("unknown .npy version")),
    };
    let header = std::str::from_utf8(b.get(start..start + hlen).ok_or_else(|| bad("short header"))?).map_err(|_| bad("header"))?;
    let compact: String = header.chars().filter(|c| !c.is_whitespace()).collect();
    if !compact.contains("'fortran_order':False") {
        return Err(bad("Fortran order is not supported"));
    }
    let shape = compact.split("'shape':(").nth(1).and_then(|r| r.split(')').next()).ok_or_else(|| bad("no shape"))?;
    let dims: Vec<usize> = shape.split(',').filter(|d| !d.is_empty()).map(|d| d.parse().map_err(|_| bad("shape"))).collect::<Result<_, _>>()?;
    let n: usize = dims.iter().product();
    let data = &b[start + hlen..];
    let out: Vec<f32> = if compact.contains("'descr':'<f4'") {
        data.chunks_exact(4).take(n).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    } else if compact.contains("'descr':'<f8'") {
        data.chunks_exact(8).take(n).map(|c| f64::from_le_bytes(c.try_into().unwrap()) as f32).collect()
    } else {
        return Err(bad("only little-endian float32 or float64 arrays are read"));
    };
    if out.len() != n {
        return Err(bad("data shorter than its shape"));
    }
    Ok(out)
}

/// `soundfile.write(path, audio, sr, subtype="FLOAT")`: mono IEEE float WAV. The samples are the same bits;
/// the file bytes differ (libsndfile adds `fact` and `PEAK` chunks, hound skips them when reading).
pub fn write_float_wav(path: &Path, audio: &[f32], sr: u32) -> Result<(), String> {
    let spec = hound::WavSpec { channels: 1, sample_rate: sr, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let e = |e: hound::Error| format!("{}: {e}", path.display());
    let mut w = hound::WavWriter::create(path, spec).map_err(e)?;
    for &x in audio {
        w.write_sample(x).map_err(e)?;
    }
    w.finalize().map_err(e)
}

/// `save_json(path, obj)`: `json.dumps(obj, indent=1)`. The same document; the text differs from Python's
/// where Python escapes non-ASCII (`ensure_ascii`) or writes an exponent as `1e-05`.
pub fn save_json(path: &Path, obj: &serde_json::Value) -> Result<(), String> {
    let mut text = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut text, serde_json::ser::PrettyFormatter::with_indent(b" "));
    serde::Serialize::serialize(obj, &mut ser).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npy_float32_and_float64_read_back() {
        let dir = tempfile_dir();
        // np.save of np.array([1.5, -2.0], np.float32) and of the float64 array: header padded to 64 bytes
        for (descr, bytes) in [("<f4", [1.5f32.to_le_bytes(), (-2.0f32).to_le_bytes()].concat()), ("<f8", [1.5f64.to_le_bytes(), (-2.0f64).to_le_bytes()].concat())] {
            let mut h = format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': (2,), }}");
            while (10 + h.len() + 1) % 64 != 0 {
                h.push(' ');
            }
            h.push('\n');
            let mut b = b"\x93NUMPY\x01\x00".to_vec();
            b.extend((h.len() as u16).to_le_bytes());
            b.extend(h.as_bytes());
            b.extend(bytes);
            let p = dir.join("fp.npy");
            std::fs::write(&p, b).unwrap();
            assert_eq!(read_npy(&p).unwrap(), vec![1.5, -2.0]);
        }
        let p = dir.join("f.wav");
        write_float_wav(&p, &[0.25, -1.5], 16000).unwrap();
        assert_eq!(crate::audio::load(&p, None).unwrap(), (vec![0.25, -1.5], 16000));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("taketwo-npy-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
