const A: f64 = 6378137.0;
const F: f64 = 1.0 / 298.257223563;
const E2: f64 = F * (2.0 - F);

pub fn ecef_to_lla(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let lon = y.atan2(x);
    let p = x.hypot(y);
    let mut lat = z.atan2(p * (1.0 - E2));
    for _ in 0..8 {
        let sin_lat = lat.sin();
        let n = A / (1.0 - E2 * sin_lat * sin_lat).sqrt();
        lat = (z + E2 * n * sin_lat).atan2(p);
    }
    let sin_lat = lat.sin();
    let n = A / (1.0 - E2 * sin_lat * sin_lat).sqrt();
    let alt = p / lat.cos() - n;
    (lon.to_degrees(), lat.to_degrees(), alt)
}

pub fn lla_to_ecef(lat_deg: f64, lon_deg: f64, alt: f64) -> (f64, f64, f64) {
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();
    let sin_lat = lat.sin();
    let cos_lat = lat.cos();
    let n = A / (1.0 - E2 * sin_lat * sin_lat).sqrt();
    let x = (n + alt) * cos_lat * lon.cos();
    let y = (n + alt) * cos_lat * lon.sin();
    let z = (n * (1.0 - E2) + alt) * sin_lat;
    (x, y, z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_equator() {
        let (lat, lon, alt) = ecef_to_lla(A, 0.0, 0.0);
        assert!((lat).abs() < 1e-6);
        assert!(lon.abs() < 1e-6);
        assert!(alt.abs() < 1e-3);
    }
}
