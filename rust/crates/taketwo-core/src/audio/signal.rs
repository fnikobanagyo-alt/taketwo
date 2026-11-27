//! Hand ports of the scipy.signal pieces audio.py uses (scipy 1.17): `butter` to second-order sections
//! (`buttap`, `lp2hp_zpk`/`lp2bp_zpk`, `bilinear_zpk`, `zpk2sos` with "nearest" pairing), `sosfiltfilt`
//! (odd padding, `sosfilt_zi` initial conditions) and `resample_poly` (Kaiser 5.0 `firwin`, zero-phase
//! polyphase filter, scipy's padding and trimming). Why hand ports: see PROGRESS.md, step S10.

use super::PyError;
use realfft::num_complex::Complex64 as C;
use std::f64::consts::PI;

pub type Sos = Vec<[f64; 6]>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BType {
    Lowpass,
    Highpass,
    Bandpass,
}

/// `scipy.signal.butter(order, wn, btype, fs=fs, output="sos")`.
pub fn butter(order: usize, wn: &[f64], btype: BType, fs: f64) -> Result<Sos, PyError> {
    let wn: Vec<f64> = wn.iter().map(|w| w / (fs / 2.0)).collect();
    if wn.iter().any(|&w| w <= 0.0 || w >= 1.0) {
        return Err(PyError::value("Digital filter critical frequencies must be 0 < Wn < fs/2"));
    }
    // buttap
    let mut p: Vec<C> = (0..order)
        .map(|i| {
            let m = (-(order as f64) + 1.0) + 2.0 * i as f64;
            -C::new(0.0, PI * m / (2.0 * order as f64)).exp()
        })
        .collect();
    let mut z: Vec<C> = Vec::new();
    let mut k = 1.0f64;
    let fs2 = 2.0; // iirfilter works at fs = 2 internally
    let warped: Vec<f64> = wn.iter().map(|w| 2.0 * fs2 * (PI * w / fs2).tan()).collect();
    let degree = p.len() - z.len();
    match btype {
        BType::Lowpass => {
            let wo = warped[0];
            p.iter_mut().for_each(|x| *x *= wo);
            k *= wo.powi(degree as i32);
        }
        BType::Highpass => {
            let wo = warped[0];
            let prod_p = p.iter().fold(C::new(1.0, 0.0), |acc, x| acc * -x);
            k *= (C::new(1.0, 0.0) / prod_p).re;
            p = p.iter().map(|x| C::new(wo, 0.0) / x).collect();
            z = vec![C::new(0.0, 0.0); degree];
        }
        BType::Bandpass => {
            let bw = warped[1] - warped[0];
            let wo = (warped[0] * warped[1]).sqrt();
            let p_lp: Vec<C> = p.iter().map(|x| x * bw / 2.0).collect();
            let root = |x: &C| (x * x - wo * wo).sqrt();
            p = p_lp.iter().map(|x| x + root(x)).chain(p_lp.iter().map(|x| x - root(x))).collect();
            z = vec![C::new(0.0, 0.0); degree];
            k *= bw.powi(degree as i32);
        }
    }
    // bilinear_zpk at fs = 2
    let f2 = 2.0 * fs2;
    let degree = p.len() - z.len();
    let num = z.iter().fold(C::new(1.0, 0.0), |acc, x| acc * (f2 - x));
    let den = p.iter().fold(C::new(1.0, 0.0), |acc, x| acc * (f2 - x));
    let mut zz: Vec<C> = z.iter().map(|x| (f2 + x) / (f2 - x)).collect();
    zz.extend(std::iter::repeat_n(C::new(-1.0, 0.0), degree));
    let pz: Vec<C> = p.iter().map(|x| (f2 + x) / (f2 - x)).collect();
    let kz = k * (num / den).re;
    Ok(zpk2sos(zz, pz, kz))
}

/// `_cplxreal`: complex values with positive imaginary part (conjugates merged), then the real ones.
fn cplxreal(z: &[C]) -> Vec<C> {
    let tol = 100.0 * f64::EPSILON;
    let mut z = z.to_vec();
    z.sort_by(|a, b| a.re.total_cmp(&b.re).then(a.im.abs().total_cmp(&b.im.abs())));
    let is_real = |x: &C| x.im.abs() <= tol * x.norm();
    let zr: Vec<C> = z.iter().filter(|x| is_real(x)).map(|x| C::new(x.re, 0.0)).collect();
    let rest: Vec<C> = z.iter().filter(|x| !is_real(x)).copied().collect();
    let mut zp: Vec<C> = rest.iter().filter(|x| x.im > 0.0).copied().collect();
    let mut zn: Vec<C> = rest.iter().filter(|x| x.im < 0.0).copied().collect();
    assert_eq!(zp.len(), zn.len(), "complex value with no matching conjugate");
    // runs of equal real part are ordered by |imag|
    let mut i = 0;
    while i < zp.len() {
        let mut j = i;
        while j + 1 < zp.len() && zp[j + 1].re - zp[j].re <= tol * zp[j].norm() {
            j += 1;
        }
        if j > i {
            zp[i..=j].sort_by(|a, b| a.im.abs().total_cmp(&b.im.abs()));
            zn[i..=j].sort_by(|a, b| a.im.abs().total_cmp(&b.im.abs()));
        }
        i = j + 1;
    }
    let mut out: Vec<C> = zp.iter().zip(&zn).map(|(a, b)| (a + b.conj()) / 2.0).collect();
    out.extend(zr);
    out
}

fn isreal(x: &C) -> bool {
    x.im == 0.0
}

/// `_nearest_real_complex_idx`.
fn nearest(fro: &[C], to: C, which: &str) -> usize {
    let mut order: Vec<usize> = (0..fro.len()).collect();
    order.sort_by(|&a, &b| (fro[a] - to).norm().total_cmp(&(fro[b] - to).norm()));
    match which {
        "any" => order[0],
        "real" => *order.iter().find(|&&i| isreal(&fro[i])).unwrap(),
        _ => *order.iter().find(|&&i| !isreal(&fro[i])).unwrap(),
    }
}

/// `np.poly` of up to two roots, real part (the roots are real or a conjugate pair).
fn poly(r: &[C]) -> Vec<f64> {
    let mut a = vec![C::new(1.0, 0.0)];
    for root in r {
        let mut b = vec![C::new(0.0, 0.0); a.len() + 1];
        for (i, x) in a.iter().enumerate() {
            b[i] += x;
            b[i + 1] += x * -root;
        }
        a = b;
    }
    a.iter().map(|x| x.re).collect()
}

fn single_zpksos(z: &[C], p: &[C]) -> [f64; 6] {
    let mut sos = [0.0; 6];
    let (b, a) = (poly(z), poly(p));
    sos[3 - b.len()..3].copy_from_slice(&b);
    sos[6 - a.len()..6].copy_from_slice(&a);
    sos
}

/// `zpk2sos(z, p, k, pairing="nearest")` for a digital filter.
fn zpk2sos(mut z: Vec<C>, mut p: Vec<C>, k: f64) -> Sos {
    if z.is_empty() && p.is_empty() {
        return vec![[k, 0.0, 0.0, 1.0, 0.0, 0.0]];
    }
    while p.len() < z.len() {
        p.push(C::new(0.0, 0.0));
    }
    while z.len() < p.len() {
        z.push(C::new(0.0, 0.0));
    }
    let n_sections = (p.len().max(z.len()) + 1) / 2;
    if p.len() % 2 == 1 {
        p.push(C::new(0.0, 0.0));
        z.push(C::new(0.0, 0.0));
    }
    let mut z = cplxreal(&z);
    let mut p = cplxreal(&p);
    let idx_worst = |p: &[C]| -> usize {
        let mut best = 0;
        for i in 1..p.len() {
            if (1.0 - p[i].norm()).abs() < (1.0 - p[best].norm()).abs() {
                best = i;
            }
        }
        best
    };
    let mut sos = vec![[0.0; 6]; n_sections];
    for si in (0..n_sections).rev() {
        let p1 = p.remove(idx_worst(&p));
        let n_real_p = p.iter().filter(|x| isreal(x)).count();
        if isreal(&p1) && n_real_p == 0 {
            let z1 = z.remove(nearest(&z, p1, "real"));
            sos[si] = single_zpksos(&[z1, C::new(0.0, 0.0)], &[p1, C::new(0.0, 0.0)]);
        } else if p.len() + 1 == z.len() && !isreal(&p1) && n_real_p == 1 && z.iter().filter(|x| isreal(x)).count() == 1 {
            let z1 = z.remove(nearest(&z, p1, "complex"));
            sos[si] = single_zpksos(&[z1, z1.conj()], &[p1, p1.conj()]);
        } else {
            let p2 = if isreal(&p1) {
                let real: Vec<usize> = (0..p.len()).filter(|&i| isreal(&p[i])).collect();
                let sub: Vec<C> = real.iter().map(|&i| p[i]).collect();
                p.remove(real[idx_worst(&sub)])
            } else {
                p1.conj()
            };
            if z.is_empty() {
                sos[si] = single_zpksos(&[], &[p1, p2]);
            } else {
                let z1 = z.remove(nearest(&z, p1, "any"));
                if !isreal(&z1) {
                    sos[si] = single_zpksos(&[z1, z1.conj()], &[p1, p2]);
                } else if !z.is_empty() {
                    let z2 = z.remove(nearest(&z, p1, "real"));
                    sos[si] = single_zpksos(&[z1, z2], &[p1, p2]);
                } else {
                    sos[si] = single_zpksos(&[z1], &[p1, p2]);
                }
            }
        }
    }
    for c in &mut sos[0][..3] {
        *c *= k;
    }
    sos
}

/// `lfilter_zi(b, a)` for one second-order section (2x2 solve with partial pivoting, as LAPACK gesv).
fn lfilter_zi(b: &[f64], a: &[f64]) -> [f64; 2] {
    let (b, a): (Vec<f64>, Vec<f64>) = if a[0] != 1.0 {
        (b.iter().map(|x| x / a[0]).collect(), a.iter().map(|x| x / a[0]).collect())
    } else {
        (b.to_vec(), a.to_vec())
    };
    let mut m = [[1.0 + a[1], -1.0], [a[2], 1.0]];
    let mut r = [b[1] - a[1] * b[0], b[2] - a[2] * b[0]];
    if m[1][0].abs() > m[0][0].abs() {
        m.swap(0, 1);
        r.swap(0, 1);
    }
    let l = m[1][0] / m[0][0];
    let u11 = m[1][1] - l * m[0][1];
    let y1 = r[1] - l * r[0];
    let x1 = y1 / u11;
    let x0 = (r[0] - m[0][1] * x1) / m[0][0];
    [x0, x1]
}

fn sosfilt_zi(sos: &Sos) -> Vec<[f64; 2]> {
    let mut scale = 1.0;
    sos.iter()
        .map(|s| {
            let zi = lfilter_zi(&s[..3], &s[3..]);
            let out = [scale * zi[0], scale * zi[1]];
            scale *= (0.0 + s[0] + s[1] + s[2]) / (0.0 + s[3] + s[4] + s[5]);
            out
        })
        .collect()
}

/// scipy's `_sosfilt` (direct form II transposed per section) with initial state `zi`.
fn sosfilt(sos: &Sos, x: &[f64], mut zi: Vec<[f64; 2]>) -> Vec<f64> {
    x.iter()
        .map(|&xn| {
            let mut cur = xn;
            for (s, z) in sos.iter().zip(zi.iter_mut()) {
                let new = s[0] * cur + z[0];
                z[0] = s[1] * cur - s[4] * new + z[1];
                z[1] = s[2] * cur - s[5] * new;
                cur = new;
            }
            cur
        })
        .collect()
}

/// `scipy.signal.sosfiltfilt(sos, x)` on float32 input (padtype "odd", default padlen); float64 output.
pub fn sosfiltfilt(sos: &Sos, x: &[f32]) -> Result<Vec<f64>, PyError> {
    let zeros = |col: usize| sos.iter().filter(|s| s[col] == 0.0).count();
    let ntaps = 2 * sos.len() + 1 - zeros(2).min(zeros(5));
    let edge = 3 * ntaps;
    let n = x.len();
    if n <= edge {
        return Err(PyError::value(&format!(
            "The length of the input vector x must be greater than padlen, which is {edge}."
        )));
    }
    // odd_ext runs on the float32 input, so the extension is float32 too
    let mut ext: Vec<f64> = Vec::with_capacity(n + 2 * edge);
    ext.extend((1..=edge).rev().map(|i| (2.0f32 * x[0] - x[i]) as f64));
    ext.extend(x.iter().map(|&v| v as f64));
    ext.extend((1..=edge).map(|i| (2.0f32 * x[n - 1] - x[n - 1 - i]) as f64));
    let zi = sosfilt_zi(sos);
    let scaled = |v: f64| zi.iter().map(|z| [z[0] * v, z[1] * v]).collect::<Vec<_>>();
    let mut y = sosfilt(sos, &ext, scaled(ext[0]));
    y.reverse();
    let mut y = sosfilt(sos, &y, scaled(y[0]));
    y.reverse();
    Ok(y[edge..edge + n].to_vec())
}

/// scipy.special.i0 by its power series (beta = 5 converges in about 25 terms).
fn i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut term, mut sum, mut k) = (1.0f64, 1.0f64, 1.0f64);
    while term > sum * 1e-17 {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// `firwin(numtaps, cutoff, window=("kaiser", beta))`, a low-pass with cutoff relative to Nyquist.
pub fn firwin_kaiser(numtaps: usize, cutoff: f64, beta: f64) -> Vec<f64> {
    let alpha = 0.5 * (numtaps as f64 - 1.0);
    let sinc = |x: f64| {
        let y = PI * if x == 0.0 { 1.0e-20 } else { x };
        y.sin() / y
    };
    let walpha = (numtaps as f64 - 1.0) / 2.0;
    let mut h: Vec<f64> = (0..numtaps)
        .map(|i| {
            let m = i as f64 - alpha;
            let win = i0(beta * (1.0 - ((i as f64 - walpha) / walpha).powf(2.0)).sqrt()) / i0(beta);
            (cutoff * sinc(cutoff * m) - 0.0 * sinc(0.0 * m)) * win
        })
        .collect();
    let s = super::np::sum_f64(&h);
    h.iter_mut().for_each(|v| *v /= s);
    h
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `scipy.signal.resample_poly(x, up, down)` on float32 input (window ("kaiser", 5.0), padtype "constant").
/// The filter is cast to float32 as scipy does; each output is summed in float64 and cast back.
pub fn resample_poly(x: &[f32], up: u64, down: u64) -> Vec<f32> {
    let g = gcd(up, down);
    let (up, down) = (up / g, down / g);
    if up == 1 && down == 1 {
        return x.to_vec();
    }
    let n_in = x.len() as u64;
    let n_out = (n_in * up).div_ceil(down);
    let max_rate = up.max(down);
    let half_len = 10 * max_rate;
    let h: Vec<f32> = firwin_kaiser((2 * half_len + 1) as usize, 1.0 / max_rate as f64, 5.0)
        .into_iter()
        .map(|v| v as f32 * up as f32)
        .collect();
    let n_pre_pad = down - half_len % down;
    let n_pre_remove = (half_len + n_pre_pad) / down;
    // upfirdn output k is sum over t of hp[k*down - t*up] * x[t], hp = n_pre_pad zeros then h
    (0..n_out)
        .map(|i| {
            let pos = (n_pre_remove + i) * down; // index into the upsampled, convolved signal
            let mut acc = 0.0f64;
            // hp index j = pos - t*up must lie in [n_pre_pad, n_pre_pad + len(h))
            let lo = pos.saturating_sub(n_pre_pad + h.len() as u64 - 1).div_ceil(up);
            let hi = if pos >= n_pre_pad { ((pos - n_pre_pad) / up).min(n_in.saturating_sub(1)) } else { return 0.0 };
            if n_in == 0 {
                return 0.0;
            }
            let mut t = lo;
            while t <= hi {
                let j = pos - t * up - n_pre_pad;
                acc += h[j as usize] as f64 * x[t as usize] as f64;
                t += 1;
            }
            acc as f32
        })
        .collect()
}
