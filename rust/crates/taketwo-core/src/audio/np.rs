//! The few numpy behaviours audio.py's numbers depend on: pairwise summation (what `np.sum` and `np.mean`
//! do along a contiguous axis), the linear-interpolation percentile, `linspace` and `hanning`.
//! Precision follows numpy 2 on float32 input: a Python float meeting a float32 array is cast to float32.

use super::PyError;

/// numpy's pairwise summation (`@TYPE@_pairwise_sum`): a plain loop under 8 values, eight accumulators up to
/// 128, else split at n/2 rounded down to a multiple of 8. A reduction starts from 0 (the add identity).
fn pairwise<T: Copy + std::ops::Add<Output = T> + Default>(a: &[T]) -> T {
    let n = a.len();
    if n < 8 {
        let mut res = T::default();
        for &x in a {
            res = res + x;
        }
        res
    } else if n <= 128 {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - n % 8 {
            for j in 0..8 {
                r[j] = r[j] + a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res = res + a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise(&a[..n2]) + pairwise(&a[n2..])
    }
}

pub fn sum_f32(a: &[f32]) -> f32 {
    pairwise(a)
}

pub fn sum_f64(a: &[f64]) -> f64 {
    pairwise(a)
}

/// `np.mean` of float32: float32 pairwise sum, divided by the count as float64 (numpy divides by an intp), cast back.
/// Empty gives NaN, as numpy.
pub fn mean_f32(a: &[f32]) -> f32 {
    (sum_f32(a) as f64 / a.len() as f64) as f32
}

pub fn mean_f64(a: &[f64]) -> f64 {
    sum_f64(a) / a.len() as f64
}

/// Sorted copy with NaN last (numpy's sort order), the index pair and gamma of the "linear" method.
fn lerp_parts<T: Copy + PartialOrd>(v: &[T], q: f64, is_nan: impl Fn(T) -> bool) -> Result<(T, T, f64, bool), PyError> {
    let n = v.len();
    if n == 0 {
        return Err(PyError::index("index -1 is out of bounds for axis 0 with size 0"));
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| match (is_nan(*a), is_nan(*b)) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
    });
    let has_nan = is_nan(s[n - 1]);
    let virt = (n - 1) as f64 * (q / 100.0);
    let (prev, next) = if virt >= (n - 1) as f64 { (n - 1, n - 1) } else { (virt.floor() as usize, virt.floor() as usize + 1) };
    let gamma = virt - virt.floor();
    Ok((s[prev], s[next], gamma, has_nan))
}

/// `np.percentile(v, q)` (linear) of float32 values: numpy 2 keeps the scalar q weak, so gamma is a Python
/// float; `_lerp` runs in float32 with it, taking `b - diff * (1 - t)` when t >= 0.5.
pub fn percentile_f32(v: &[f32], q: f64) -> Result<f32, PyError> {
    let (a, b, t, nan) = lerp_parts(v, q, f32::is_nan)?;
    if nan {
        return Ok(f32::NAN);
    }
    let diff = b - a;
    Ok(if t >= 0.5 { b - diff * ((1.0 - t) as f32) } else { a + diff * (t as f32) })
}

/// `np.percentile(v, q)` (linear) of float64 values.
pub fn percentile_f64(v: &[f64], q: f64) -> Result<f64, PyError> {
    let (a, b, t, nan) = lerp_parts(v, q, f64::is_nan)?;
    if nan {
        return Ok(f64::NAN);
    }
    let diff = b - a;
    Ok(if t >= 0.5 { b - diff * (1.0 - t) } else { a + diff * t })
}

/// `np.linspace(start, stop, n, dtype=np.float32)`: computed in float64, last value set to `stop`, then cast.
pub fn linspace_f32(start: f64, stop: f64, n: usize) -> Vec<f32> {
    let delta = stop - start;
    if n == 0 {
        return Vec::new();
    }
    let div = n - 1;
    let mut y: Vec<f64> = (0..n).map(|i| i as f64).collect();
    if div > 0 {
        let step = delta / div as f64;
        if step == 0.0 {
            y.iter_mut().for_each(|v| *v = *v / div as f64 * delta);
        } else {
            y.iter_mut().for_each(|v| *v *= step);
        }
    } else {
        y.iter_mut().for_each(|v| *v *= delta);
    }
    y.iter_mut().for_each(|v| *v += start);
    if n > 1 {
        y[n - 1] = stop;
    }
    y.into_iter().map(|v| v as f32).collect()
}

/// `np.hanning(m)`.
pub fn hanning(m: usize) -> Vec<f64> {
    match m {
        0 => Vec::new(),
        1 => vec![1.0],
        _ => (0..m)
            .map(|k| {
                let n = (1 - m as i64 + 2 * k as i64) as f64;
                0.5 + 0.5 * (std::f64::consts::PI * n / (m - 1) as f64).cos()
            })
            .collect(),
    }
}

/// Python slice bounds `a[lo:hi]` for a length (negative indexes count from the end; out of range clamps).
pub fn py_slice(len: usize, lo: i64, hi: i64) -> std::ops::Range<usize> {
    let fix = |i: i64| -> usize {
        let i = if i < 0 { i + len as i64 } else { i };
        i.clamp(0, len as i64) as usize
    };
    let (lo, hi) = (fix(lo), fix(hi));
    lo..hi.max(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_np_helpers() {
        assert_eq!(linspace_f32(0.0, 1.0, 1), vec![0.0]);
        assert_eq!(linspace_f32(1.0, 0.0, 1), vec![1.0]);
        assert_eq!(linspace_f32(0.0, 1.0, 3), vec![0.0, 0.5, 1.0]);
        assert_eq!(percentile_f32(&[1.0, 2.0, 3.0, 4.0, 5.0], 50.0).unwrap(), 3.0);
        assert!(percentile_f32(&[], 10.0).is_err());
        assert_eq!(py_slice(10, 3, 2), 3..3);
        assert_eq!(py_slice(10, -3, 20), 7..10);
        assert_eq!(hanning(3), vec![0.0, 1.0, 0.0]);
        let v: Vec<f32> = (0..1000).map(|i| i as f32 * 0.1).collect();
        assert!((sum_f32(&v) - 49950.0).abs() < 0.1);
    }
}
