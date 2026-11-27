//! Hand port of pyloudnorm 0.2.0 `Meter(rate).integrated_loudness` for mono float32 audio: the K-weighting
//! biquads (RBJ cookbook high shelf and high pass, run by `lfilter`, the result stored back as float32),
//! 400 ms blocks with 75 % overlap (pyloudnorm's own block count and bounds, a short last block still
//! divided by the full block length), the absolute -70 LUFS gate and the relative -10 LU gate.

use super::np;

fn biquad(g: f64, q: f64, fc: f64, rate: f64, high_shelf: bool) -> ([f64; 3], [f64; 3]) {
    let a = 10f64.powf(g / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * (fc / rate);
    let alpha = w0.sin() / (2.0 * q);
    let c = w0.cos();
    let (b, aa) = if high_shelf {
        (
            [
                a * ((a + 1.0) + (a - 1.0) * c + 2.0 * a.sqrt() * alpha),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - 2.0 * a.sqrt() * alpha),
            ],
            [
                (a + 1.0) - (a - 1.0) * c + 2.0 * a.sqrt() * alpha,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - 2.0 * a.sqrt() * alpha,
            ],
        )
    } else {
        ([(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0], [1.0 + alpha, -2.0 * c, 1.0 - alpha])
    };
    let a0 = aa[0];
    (b.map(|v| v / a0), aa.map(|v| v / a0))
}

/// scipy `lfilter(b, a, x)` (direct form II transposed), float64, then stored into the float32 buffer.
fn lfilter_into(b: [f64; 3], a: [f64; 3], x: &mut [f32]) {
    let (b, a) = (b.map(|v| v / a[0]), a.map(|v| v / a[0]));
    let (mut z0, mut z1) = (0.0f64, 0.0f64);
    for v in x.iter_mut() {
        let xn = *v as f64;
        let y = z0 + b[0] * xn;
        z0 = z1 + b[1] * xn - a[1] * y;
        z1 = b[2] * xn - a[2] * y;
        *v = y as f32;
    }
}

/// pyloudnorm's integrated loudness (LUFS) of mono audio. -inf when no block passes the gates.
pub fn integrated_loudness(data: &[f32], rate: u32) -> f64 {
    let rate_f = rate as f64;
    let mut x = data.to_vec();
    let (b, a) = biquad(4.0, 1.0 / 2f64.sqrt(), 1500.0, rate_f, true);
    lfilter_into(b, a, &mut x);
    let (b, a) = biquad(0.0, 0.5, 38.0, rate_f, false);
    lfilter_into(b, a, &mut x);

    let t_g = 0.400;
    let step = 1.0 - 0.75;
    let t = x.len() as f64 / rate_f;
    let num_blocks = (((t - t_g) / (t_g * step)).round_ties_even() as i64 + 1).max(0) as usize;
    let norm = (1.0 / (t_g * rate_f)) as f32;
    let z: Vec<f64> = (0..num_blocks)
        .map(|j| {
            let l = (t_g * (j as f64 * step) * rate_f) as i64;
            let u = (t_g * (j as f64 * step + 1.0) * rate_f) as i64;
            let r = np::py_slice(x.len(), l, u);
            let sq: Vec<f32> = x[r].iter().map(|v| v * v).collect();
            (norm * np::sum_f32(&sq)) as f64
        })
        .collect();
    let lj: Vec<f64> = z.iter().map(|&zj| -0.691 + 10.0 * (1.0 * zj).log10()).collect();
    let gated = |keep: &dyn Fn(f64) -> bool| -> Vec<f64> { z.iter().zip(&lj).filter(|(_, l)| keep(**l)).map(|(z, _)| *z).collect() };
    let abs_g = gated(&|l| l >= -70.0);
    let gamma_r = -0.691 + 10.0 * (1.0 * np::mean_f64(&abs_g)).log10() - 10.0;
    let rel_g = gated(&|l| l > gamma_r && l > -70.0);
    let mut avg = np::mean_f64(&rel_g);
    if avg.is_nan() {
        avg = 0.0; // np.nan_to_num
    }
    -0.691 + 10.0 * (1.0 * avg).log10()
}
