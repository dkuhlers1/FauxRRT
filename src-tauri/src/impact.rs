//! Last downward crossing of a WGS-84 HAE threshold (default 0 m).
//! O(n) scan, linear interpolation, dateline-safe longitude.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactPoint {
    pub lon: f64,
    pub lat: f64,
    pub alt: f64,
    pub time: Option<f64>,
    pub index: usize,
    pub fraction: f64,
}

/// Packed LLA is `[lon, lat, alt, …]` metres HAE.
pub fn extract_impact(lla: &[f32], times: Option<&[f64]>, threshold_m: f32) -> Option<ImpactPoint> {
    let n = lla.len() / 3;
    if n < 2 {
        return None;
    }

    let mut max_alt = f32::NEG_INFINITY;
    let mut min_alt = f32::INFINITY;
    let mut apo_i = 0usize;
    for i in 0..n {
        let a = lla[i * 3 + 2];
        if a >= max_alt {
            max_alt = a;
            apo_i = i;
        }
        min_alt = min_alt.min(a);
    }
    // Flat / missing-altitude tracks never left the surface.
    if max_alt <= threshold_m + 1.0 || max_alt - min_alt < 20.0 {
        return None;
    }

    // Prefer a HAE threshold crossing (sea-level / ellipsoid).
    if let Some(i) = last_downward_crossing(lla, n, threshold_m) {
        return Some(interpolate_crossing(lla, times, n, i, threshold_m as f64));
    }

    // Terminal landing after apogee: pad elevation, terrain above the ellipsoid,
    // or a propagator that stops a couple of metres above the floor.
    let last = n - 1;
    let last_alt = lla[last * 3 + 2];
    if last <= apo_i || max_alt - last_alt < 20.0 {
        return None;
    }
    for i in (apo_i..last).rev() {
        let a0 = lla[i * 3 + 2];
        let a1 = lla[(i + 1) * 3 + 2];
        if a0 > a1 {
            return Some(interpolate_crossing(lla, times, n, i, a1 as f64));
        }
    }
    Some(interpolate_crossing(lla, times, n, last - 1, last_alt as f64))
}

fn last_downward_crossing(lla: &[f32], n: usize, floor: f32) -> Option<usize> {
    let mut last = None;
    for i in 0..n - 1 {
        let a0 = lla[i * 3 + 2];
        let a1 = lla[(i + 1) * 3 + 2];
        if a0 > floor && a1 <= floor {
            last = Some(i);
        }
    }
    last
}

fn interpolate_crossing(
    lla: &[f32],
    times: Option<&[f64]>,
    n: usize,
    i: usize,
    floor: f64,
) -> ImpactPoint {
    let a0 = lla[i * 3 + 2] as f64;
    let a1 = lla[(i + 1) * 3 + 2] as f64;
    let denom = a0 - a1;
    let frac = if denom.abs() < 1e-12 {
        1.0
    } else {
        ((a0 - floor) / denom).clamp(0.0, 1.0)
    };

    let lon = lerp_lon(lla[i * 3] as f64, lla[(i + 1) * 3] as f64, frac);
    let lat = lerp(lla[i * 3 + 1] as f64, lla[(i + 1) * 3 + 1] as f64, frac);
    let alt = lerp(a0, a1, frac);
    let time = match times {
        Some(t) if t.len() == n => Some(lerp(t[i], t[i + 1], frac)),
        _ => None,
    };

    ImpactPoint {
        lon,
        lat,
        alt,
        time,
        index: i,
        fraction: frac,
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_lon(a: f64, b: f64, t: f64) -> f64 {
    let mut d = b - a;
    if d > 180.0 {
        d -= 360.0;
    } else if d < -180.0 {
        d += 360.0;
    }
    let mut x = a + t * d;
    if x > 180.0 {
        x -= 360.0;
    } else if x < -180.0 {
        x += 360.0;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lla_from(points: &[(f32, f32, f32)]) -> Vec<f32> {
        let mut v = Vec::with_capacity(points.len() * 3);
        for (lon, lat, alt) in points {
            v.push(*lon);
            v.push(*lat);
            v.push(*alt);
        }
        v
    }

    #[test]
    fn interpolates_last_downward_crossing() {
        let lla = lla_from(&[
            (10.0, 20.0, 1000.0),
            (10.1, 20.1, 500.0),
            (10.2, 20.2, 100.0),
            (10.3, 20.3, -20.0),
        ]);
        let hit = extract_impact(&lla, None, 0.0).unwrap();
        assert_eq!(hit.index, 2);
        let expected_frac = 100.0 / 120.0;
        assert!((hit.fraction - expected_frac).abs() < 1e-6);
        assert!((hit.lon - (10.2 + 0.1 * expected_frac)).abs() < 1e-5);
        assert!(hit.alt.abs() < 1e-6);
    }

    #[test]
    fn ignores_ascent_and_overflight() {
        let climb = lla_from(&[(0.0, 0.0, 10.0), (0.1, 0.0, 1000.0), (0.2, 0.0, 2000.0)]);
        assert!(extract_impact(&climb, None, 0.0).is_none());

        let ground = lla_from(&[(0.0, 0.0, 0.0), (0.1, 0.0, 0.0), (0.2, 0.0, 0.0)]);
        assert!(extract_impact(&ground, None, 0.0).is_none());
    }

    #[test]
    fn uses_last_crossing_if_multiple() {
        let lla = lla_from(&[
            (0.0, 0.0, 100.0),
            (0.1, 0.0, -10.0),
            (0.2, 0.0, 80.0),
            (0.3, 0.0, -10.0),
        ]);
        let hit = extract_impact(&lla, None, 0.0).unwrap();
        assert_eq!(hit.index, 2);
    }

    #[test]
    fn interpolates_across_dateline() {
        let lla = lla_from(&[(179.8, 10.0, 50.0), (-179.8, 10.0, -50.0)]);
        let hit = extract_impact(&lla, None, 0.0).unwrap();
        assert!(hit.lon.abs() > 179.0 || hit.lon.abs() < 1.0);
        assert!((hit.fraction - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pad_landing_above_ellipsoid_counts() {
        let lla = lla_from(&[
            (-106.97, 32.99, 1400.0),
            (-106.96, 33.01, 4200.0),
            (-106.90, 33.10, 2100.0),
            (-106.88, 33.12, 1405.0),
        ]);
        let hit = extract_impact(&lla, None, 0.0).unwrap();
        assert_eq!(hit.index, 2);
        assert!((hit.alt - 1405.0).abs() < 1.0);
    }

    #[test]
    fn landing_above_launch_altitude_counts() {
        let lla = lla_from(&[
            (-106.4, 32.4, 0.0),
            (-106.3, 32.6, 80_000.0),
            (-105.5, 33.2, 12_000.0),
            (-105.2, 33.4, 1400.0),
        ]);
        let hit = extract_impact(&lla, None, 0.0).unwrap();
        assert!((hit.alt - 1400.0).abs() < 1.0);
    }

    #[test]
    fn propagator_stop_just_above_ellipsoid_counts() {
        let lla = lla_from(&[
            (-97.27, 19.43, 0.0),
            (-96.5, 19.6, 40_000.0),
            (-95.5, 19.9, 800.0),
            (-95.35, 19.95, 1.8),
        ]);
        let hit = extract_impact(&lla, None, 0.0).unwrap();
        assert!(hit.alt < 5.0);
    }
}
