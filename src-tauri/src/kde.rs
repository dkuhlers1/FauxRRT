//! Weighted 2D Gaussian KDE of impact points.
//!
//! Bandwidth is Botev's improved Sheather–Jones (ISJ) per east/north axis,
//! with a robust Silverman fallback. Smoothing uses an FFT convolution so
//! large Monte Carlo sets stay interactive.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::Serialize;

const MIN_BW_M: f64 = 250.0;
const MIN_BW_SPARSE_M: f64 = 2_000.0;
const MIN_SPAN_M: f64 = 8_000.0;
const ISJ_BINS: usize = 512;
const FFT_GRID: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct WeightedPoint {
    pub lon: f64,
    pub lat: f64,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KdeGrid {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
    pub nx: usize,
    pub ny: usize,
    /// Probability density per m², row 0 = north (image order).
    pub values: Vec<f32>,
    pub max_value: f32,
    pub mass: f64,
    pub bandwidth_east_m: f64,
    pub bandwidth_north_m: f64,
    pub n_points: usize,
}

pub fn kde_lonlat(points: &[WeightedPoint]) -> Option<KdeGrid> {
    let pts: Vec<_> = points.iter().filter(|p| p.weight > 0.0 && p.lat.abs() <= 90.0).copied().collect();
    if pts.is_empty() {
        return None;
    }

    let mass: f64 = pts.iter().map(|p| p.weight).sum();
    if mass <= 0.0 {
        return None;
    }

    let lat0 = pts.iter().map(|p| p.lat * p.weight).sum::<f64>() / mass;
    let lon0 = circular_mean_lon(&pts, mass);
    let mx = metres_per_deg_lon(lat0);
    let my = metres_per_deg_lat();

    let mut xs = Vec::with_capacity(pts.len());
    let mut ys = Vec::with_capacity(pts.len());
    let mut ws = Vec::with_capacity(pts.len());
    for p in &pts {
        xs.push((wrap_lon(p.lon - lon0)) * mx);
        ys.push((p.lat - lat0) * my);
        ws.push(p.weight);
    }

    let n_eff = effective_n(&ws);
    let min_bw = if n_eff < 12.0 { MIN_BW_SPARSE_M } else { MIN_BW_M };
    let hx = automatic_bandwidth(&xs, &ws, n_eff).max(min_bw);
    let hy = automatic_bandwidth(&ys, &ws, n_eff).max(min_bw);

    let pad = 4.0 * hx.max(hy);
    let mut xmin = xs.iter().copied().fold(f64::INFINITY, f64::min) - pad;
    let mut xmax = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max) + pad;
    let mut ymin = ys.iter().copied().fold(f64::INFINITY, f64::min) - pad;
    let mut ymax = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max) + pad;
    if xmax - xmin < MIN_SPAN_M {
        let extra = (MIN_SPAN_M - (xmax - xmin)) * 0.5;
        xmin -= extra;
        xmax += extra;
    }
    if ymax - ymin < MIN_SPAN_M {
        let extra = (MIN_SPAN_M - (ymax - ymin)) * 0.5;
        ymin -= extra;
        ymax += extra;
    }

    let nx = FFT_GRID;
    let ny = FFT_GRID;
    let dx = (xmax - xmin) / nx as f64;
    let dy = (ymax - ymin) / ny as f64;
    if dx <= 0.0 || dy <= 0.0 {
        return None;
    }

    let mut hist = vec![0.0f64; nx * ny];
    for i in 0..pts.len() {
        let fx = (xs[i] - xmin) / dx;
        let fy = (ys[i] - ymin) / dy;
        splat_bilinear(&mut hist, nx, ny, fx, fy, ws[i]);
    }

    convolve_gaussian(&mut hist, nx, ny, dx, dy, hx, hy);

    let cell = dx * dy;
    let mut values = vec![0.0f32; nx * ny];
    let mut max_value = 0.0f32;
    let mut recovered = 0.0f64;
    for j in 0..ny {
        let src_row = j;
        let dst_row = ny - 1 - j; // north-up
        for i in 0..nx {
            let dens = (hist[src_row * nx + i] / cell).max(0.0);
            recovered += dens * cell;
            let v = dens as f32;
            values[dst_row * nx + i] = v;
            if v > max_value {
                max_value = v;
            }
        }
    }

    let west = lon0 + xmin / mx;
    let east = lon0 + xmax / mx;
    let south = lat0 + ymin / my;
    let north = lat0 + ymax / my;

    Some(KdeGrid {
        west,
        south,
        east,
        north,
        nx,
        ny,
        values,
        max_value,
        mass: recovered,
        bandwidth_east_m: hx,
        bandwidth_north_m: hy,
        n_points: pts.len(),
    })
}

/// Bilinear sample of probability density (1/m²). Zero outside the grid.
pub fn sample_density(grid: &KdeGrid, lon: f64, lat: f64) -> f64 {
    if grid.nx < 2 || grid.ny < 2 {
        return 0.0;
    }
    let dw = grid.east - grid.west;
    let dh = grid.north - grid.south;
    if dw <= 0.0 || dh <= 0.0 {
        return 0.0;
    }
    let fx = ((lon - grid.west) / dw) * grid.nx as f64 - 0.5;
    let fy = ((grid.north - lat) / dh) * grid.ny as f64 - 0.5;
    if !(0.0..=(grid.nx - 1) as f64).contains(&fx) || !(0.0..=(grid.ny - 1) as f64).contains(&fy) {
        return 0.0;
    }
    let i0 = fx.floor() as usize;
    let j0 = fy.floor() as usize;
    let i1 = (i0 + 1).min(grid.nx - 1);
    let j1 = (j0 + 1).min(grid.ny - 1);
    let tx = fx - i0 as f64;
    let ty = fy - j0 as f64;
    let v = |i: usize, j: usize| grid.values[j * grid.nx + i] as f64;
    (1.0 - ty) * ((1.0 - tx) * v(i0, j0) + tx * v(i1, j0))
        + ty * ((1.0 - tx) * v(i0, j1) + tx * v(i1, j1))
}

fn splat_bilinear(hist: &mut [f64], nx: usize, ny: usize, fx: f64, fy: f64, w: f64) {
    if fx < 0.0 || fy < 0.0 || fx >= nx as f64 - 1e-9 || fy >= ny as f64 - 1e-9 {
        return;
    }
    let i0 = fx.floor() as usize;
    let j0 = fy.floor() as usize;
    let tx = fx - i0 as f64;
    let ty = fy - j0 as f64;
    let i1 = (i0 + 1).min(nx - 1);
    let j1 = (j0 + 1).min(ny - 1);
    hist[j0 * nx + i0] += w * (1.0 - tx) * (1.0 - ty);
    hist[j0 * nx + i1] += w * tx * (1.0 - ty);
    hist[j1 * nx + i0] += w * (1.0 - tx) * ty;
    hist[j1 * nx + i1] += w * tx * ty;
}

fn convolve_gaussian(hist: &mut [f64], nx: usize, ny: usize, dx: f64, dy: f64, hx: f64, hy: f64) {
    let n = nx * ny;
    let mut spec: Vec<Complex<f64>> = hist.iter().map(|&v| Complex::new(v, 0.0)).collect();

    let mut planner = FftPlanner::new();
    let fft_x = planner.plan_fft_forward(nx);
    let fft_y = planner.plan_fft_forward(ny);
    let ifft_x = planner.plan_fft_inverse(nx);
    let ifft_y = planner.plan_fft_inverse(ny);

    for y in 0..ny {
        fft_x.process(&mut spec[y * nx..(y + 1) * nx]);
    }
    let mut col = vec![Complex::new(0.0, 0.0); ny];
    for x in 0..nx {
        for y in 0..ny {
            col[y] = spec[y * nx + x];
        }
        fft_y.process(&mut col);
        for y in 0..ny {
            spec[y * nx + x] = col[y];
        }
    }

    for y in 0..ny {
        let fy = fft_freq(y, ny, dy);
        for x in 0..nx {
            let fx = fft_freq(x, nx, dx);
            let gain = (-2.0 * std::f64::consts::PI * std::f64::consts::PI * (hx * hx * fx * fx + hy * hy * fy * fy)).exp();
            spec[y * nx + x] *= gain;
        }
    }

    for x in 0..nx {
        for y in 0..ny {
            col[y] = spec[y * nx + x];
        }
        ifft_y.process(&mut col);
        for y in 0..ny {
            spec[y * nx + x] = col[y];
        }
    }
    for y in 0..ny {
        ifft_x.process(&mut spec[y * nx..(y + 1) * nx]);
    }

    let scale = 1.0 / n as f64;
    for i in 0..n {
        hist[i] = spec[i].re * scale;
    }
}

fn fft_freq(k: usize, n: usize, step: f64) -> f64 {
    let k = if k <= n / 2 { k as i64 } else { k as i64 - n as i64 };
    k as f64 / (n as f64 * step)
}

fn automatic_bandwidth(x: &[f64], w: &[f64], n_eff: f64) -> f64 {
    isj_bandwidth(x, w).unwrap_or_else(|| silverman_bandwidth(x, w, n_eff))
}

fn silverman_bandwidth(x: &[f64], w: &[f64], n_eff: f64) -> f64 {
    let sigma = weighted_std(x, w).max(iqr_sigma(x, w));
    if sigma <= 0.0 {
        return 500.0;
    }
    let n = n_eff.max(2.0);
    0.9 * sigma * n.powf(-0.2)
}

fn isj_bandwidth(x: &[f64], w: &[f64]) -> Option<f64> {
    let mut xmin = f64::INFINITY;
    let mut xmax = f64::NEG_INFINITY;
    for i in 0..x.len() {
        if w[i] <= 0.0 {
            continue;
        }
        xmin = xmin.min(x[i]);
        xmax = xmax.max(x[i]);
    }
    if !xmin.is_finite() {
        return None;
    }
    let range = (xmax - xmin).max(1.0);
    let min = xmin - 0.5 * range;
    let max = xmax + 0.5 * range;
    let r = max - min;
    if r <= 0.0 {
        return None;
    }

    let n = ISJ_BINS;
    let mut hist = vec![0.0; n];
    let dx = r / (n as f64 - 1.0);
    for i in 0..x.len() {
        if w[i] <= 0.0 {
            continue;
        }
        let pos = (x[i] - min) / dx;
        let j = pos.floor();
        let f = pos - j;
        let j0 = j as isize;
        if j0 >= 0 && (j0 as usize) < n {
            hist[j0 as usize] += w[i] * (1.0 - f);
        }
        let j1 = j0 + 1;
        if j1 >= 0 && (j1 as usize) < n {
            hist[j1 as usize] += w[i] * f;
        }
    }
    let sum: f64 = hist.iter().sum();
    if sum <= 0.0 {
        return None;
    }
    for h in &mut hist {
        *h /= sum;
    }

    let a = dct2_scipy(&hist);
    let mut i_sq = Vec::with_capacity(n - 1);
    let mut a2 = Vec::with_capacity(n - 1);
    for k in 1..n {
        i_sq.push((k as f64) * (k as f64));
        a2.push(a[k] * a[k]);
    }

    let n_unique = unique_count(x, w).max(2) as f64;
    let t_star = isj_root(n_unique, &i_sq, &a2)?;
    let bw = t_star.sqrt() * r;
    if bw.is_finite() && bw > 0.0 {
        Some(bw)
    } else {
        None
    }
}

fn dct2_scipy(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let mut y = vec![0.0; n];
    let scale = std::f64::consts::PI / (2.0 * n as f64);
    for k in 0..n {
        let mut sum = 0.0;
        for (ni, &v) in x.iter().enumerate() {
            sum += v * ((2.0 * ni as f64 + 1.0) * k as f64 * scale).cos();
        }
        y[k] = 2.0 * sum;
    }
    y
}

fn isj_fixed_point(t: f64, n: f64, i_sq: &[f64], a2: &[f64]) -> f64 {
    let ell = 7i32;
    let mut f = 0.5 * std::f64::consts::PI.powi(2 * ell)
        * i_sq
            .iter()
            .zip(a2)
            .map(|(i, a)| i.powi(ell) * a * (-i * std::f64::consts::PI * std::f64::consts::PI * t).exp())
            .sum::<f64>();
    if f <= 0.0 {
        return -1.0;
    }
    for s in (2..ell).rev() {
        let mut odd = 1.0;
        let mut k = 1.0;
        while k <= (2 * s) as f64 {
            odd *= k;
            k += 2.0;
        }
        let k0 = odd / (2.0 * std::f64::consts::PI).sqrt();
        let const_s = (1.0 + 0.5_f64.powf(s as f64 + 0.5)) / 3.0;
        let time = (2.0 * const_s * k0 / (n * f)).powf(2.0 / (3.0 + 2.0 * s as f64));
        f = 0.5
            * std::f64::consts::PI.powi(2 * s)
            * i_sq
                .iter()
                .zip(a2)
                .map(|(i, a)| i.powi(s) * a * (-i * std::f64::consts::PI * std::f64::consts::PI * time).exp())
                .sum::<f64>();
        if f <= 0.0 {
            return -1.0;
        }
    }
    let t_opt = (2.0 * n * std::f64::consts::PI.sqrt() * f).powf(-2.0 / 5.0);
    t - t_opt
}

fn isj_root(n: f64, i_sq: &[f64], a2: &[f64]) -> Option<f64> {
    let n_clip = n.clamp(50.0, 1050.0);
    let mut tol = 1e-11 + 0.01 * (n_clip - 50.0) / 1000.0;
    for _ in 0..48 {
        if let Some(x) = bisect_root(0.0, tol, |t| isj_fixed_point(t, n, i_sq, a2)) {
            if x > 0.0 {
                return Some(x);
            }
        }
        tol *= 2.0;
        if tol >= 1.0 {
            break;
        }
    }
    None
}

fn bisect_root(mut lo: f64, mut hi: f64, f: impl Fn(f64) -> f64) -> Option<f64> {
    let mut flo = f(lo);
    let mut fhi = f(hi);
    if !flo.is_finite() || !fhi.is_finite() {
        return None;
    }
    if flo == 0.0 {
        return Some(lo);
    }
    if fhi == 0.0 {
        return Some(hi);
    }
    if flo * fhi > 0.0 {
        return None;
    }
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        let fmid = f(mid);
        if !fmid.is_finite() {
            return None;
        }
        if fmid == 0.0 || (hi - lo).abs() < 1e-18 {
            return Some(mid);
        }
        if flo * fmid <= 0.0 {
            hi = mid;
            fhi = fmid;
        } else {
            lo = mid;
            flo = fmid;
        }
        let _ = fhi;
    }
    Some(0.5 * (lo + hi))
}

fn effective_n(w: &[f64]) -> f64 {
    let sum: f64 = w.iter().sum();
    let sumsq: f64 = w.iter().map(|v| v * v).sum();
    if sumsq <= 0.0 {
        0.0
    } else {
        (sum * sum) / sumsq
    }
}

fn weighted_std(x: &[f64], w: &[f64]) -> f64 {
    let mass: f64 = w.iter().sum();
    if mass <= 0.0 {
        return 0.0;
    }
    let mean = x.iter().zip(w).map(|(v, wt)| v * wt).sum::<f64>() / mass;
    let var = x.iter().zip(w).map(|(v, wt)| wt * (v - mean) * (v - mean)).sum::<f64>() / mass;
    var.max(0.0).sqrt()
}

fn iqr_sigma(x: &[f64], w: &[f64]) -> f64 {
    let mut pairs: Vec<(f64, f64)> = x.iter().zip(w).filter(|(_, wt)| **wt > 0.0).map(|(v, wt)| (*v, *wt)).collect();
    if pairs.len() < 4 {
        return 0.0;
    }
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let q1 = weighted_quantile(&pairs, 0.25);
    let q3 = weighted_quantile(&pairs, 0.75);
    (q3 - q1) / 1.3489795003921634
}

fn weighted_quantile(sorted: &[(f64, f64)], q: f64) -> f64 {
    let total: f64 = sorted.iter().map(|(_, w)| *w).sum();
    let target = q * total;
    let mut acc = 0.0;
    for (v, w) in sorted {
        acc += *w;
        if acc >= target {
            return *v;
        }
    }
    sorted.last().map(|(v, _)| *v).unwrap_or(0.0)
}

fn unique_count(x: &[f64], w: &[f64]) -> usize {
    let mut vals: Vec<i64> = x
        .iter()
        .zip(w)
        .filter(|(_, wt)| **wt > 0.0)
        .map(|(v, _)| (v * 100.0).round() as i64)
        .collect();
    vals.sort_unstable();
    vals.dedup();
    vals.len()
}

fn circular_mean_lon(pts: &[WeightedPoint], mass: f64) -> f64 {
    let mut sx = 0.0;
    let mut sy = 0.0;
    for p in pts {
        let r = p.lon.to_radians();
        sx += p.weight * r.cos();
        sy += p.weight * r.sin();
    }
    if sx.abs() < 1e-18 && sy.abs() < 1e-18 {
        return pts[0].lon;
    }
    let _ = mass;
    sy.atan2(sx).to_degrees()
}

fn wrap_lon(d: f64) -> f64 {
    let mut x = d;
    while x > 180.0 {
        x -= 360.0;
    }
    while x < -180.0 {
        x += 360.0;
    }
    x
}

fn metres_per_deg_lon(lat_deg: f64) -> f64 {
    111_132.0 * lat_deg.to_radians().cos().abs().max(0.15)
}

fn metres_per_deg_lat() -> f64 {
    111_132.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_mass_and_peaks_near_cluster() {
        let mut pts = Vec::new();
        for i in 0..40 {
            let jitter = (i as f64 - 20.0) * 0.002;
            pts.push(WeightedPoint {
                lon: -106.4 + jitter,
                lat: 32.4 + jitter * 0.4,
                weight: 0.025,
            });
        }
        let grid = kde_lonlat(&pts).expect("kde");
        assert!(grid.mass > 0.7 && grid.mass < 1.3, "mass {}", grid.mass);
        assert!(grid.bandwidth_east_m > 0.0);
        assert!(grid.max_value > 0.0);

        let mut peak = 0usize;
        for (i, v) in grid.values.iter().enumerate() {
            if *v > grid.values[peak] {
                peak = i;
            }
        }
        let row = peak / grid.nx;
        let col = peak % grid.nx;
        let lon = grid.west + (col as f64 + 0.5) / grid.nx as f64 * (grid.east - grid.west);
        let lat = grid.north - (row as f64 + 0.5) / grid.ny as f64 * (grid.north - grid.south);
        assert!((lon + 106.4).abs() < 0.15, "lon {lon}");
        assert!((lat - 32.4).abs() < 0.15, "lat {lat}");
        let at_peak = sample_density(&grid, -106.4, 32.4);
        let far = sample_density(&grid, -90.0, 20.0);
        assert!(at_peak > far * 10.0, "peak {at_peak} far {far}");
    }

    #[test]
    fn single_impact_makes_a_visible_grid() {
        let grid = kde_lonlat(&[WeightedPoint {
            lon: -95.35,
            lat: 19.95,
            weight: 1.0,
        }])
        .expect("kde");
        assert!(grid.max_value > 0.0);
        assert!(grid.east - grid.west > 0.05, "span {}", grid.east - grid.west);
        assert!(grid.north - grid.south > 0.05);
        assert!(grid.bandwidth_east_m >= 1_900.0);
    }

    #[test]
    fn silverman_positive_for_spread_data() {
        let x: Vec<f64> = (0..20).map(|i| i as f64).collect();
        let w = vec![1.0; 20];
        let h = silverman_bandwidth(&x, &w, 20.0);
        assert!(h > 0.5);
    }
}
