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

/// Local east/north/up metres to geodetic coordinates.
pub fn enu_to_lla(lat0: f64, lon0: f64, alt0: f64, east: f64, north: f64, up: f64) -> (f64, f64, f64) {
    let (x0, y0, z0) = lla_to_ecef(lat0, lon0, alt0);
    let lat = lat0.to_radians();
    let lon = lon0.to_radians();
    let (sl, cl) = (lat.sin(), lat.cos());
    let (so, co) = (lon.sin(), lon.cos());
    let x = x0 + (-so) * east + (-sl * co) * north + (cl * co) * up;
    let y = y0 + co * east + (-sl * so) * north + (cl * so) * up;
    let z = z0 + cl * north + sl * up;
    ecef_to_lla(x, y, z)
}

/// Earth-fixed from inertial by a GMST (or elapsed earth-rotation) angle.
pub fn eci_to_ecef(x: f64, y: f64, z: f64, theta_rad: f64) -> (f64, f64, f64) {
    let (c, s) = (theta_rad.cos(), theta_rad.sin());
    (c * x + s * y, -s * x + c * y, z)
}

pub fn gmst_rad(unix_s: f64) -> f64 {
    let jd = unix_s / 86400.0 + 2440587.5;
    let deg = 280.46061837 + 360.98564736629 * (jd - 2451545.0);
    deg.rem_euclid(360.0).to_radians()
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

    #[test]
    fn enu_zero_offset_stays_on_the_origin() {
        let (lon, lat, alt) = enu_to_lla(32.4, -106.4, 1200.0, 0.0, 0.0, 0.0);
        assert!((lat - 32.4).abs() < 1e-6);
        assert!((lon + 106.4).abs() < 1e-6);
        assert!((alt - 1200.0).abs() < 0.05);
    }

    #[test]
    fn eci_at_zero_angle_matches_ecef() {
        let (x, y, z) = eci_to_ecef(-2_000_000.0, 5_000_000.0, 3_000_000.0, 0.0);
        assert!((x + 2_000_000.0).abs() < 1e-6);
        assert!((y - 5_000_000.0).abs() < 1e-6);
        assert!((z - 3_000_000.0).abs() < 1e-6);
    }
}
