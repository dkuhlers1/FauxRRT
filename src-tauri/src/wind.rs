//! Horizontal winds for the 3DOF generator.
//!
//! Constant wind is a single meteorological (FROM) vector at every height.
//! Historical wind pulls GFS pressure-level soundings at launch, midpoint,
//! and aim from Open-Meteo (coverage from 2021). Aloft values fade to zero
//! above the top of the sounding.

use serde::{Deserialize, Serialize};
use serde_json::Value;

const FADE_ABOVE_M: f64 = 20_000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WindSpec {
    Off,
    Constant { speed_mps: f64, from_deg: f64 },
    Historical {
        date: String,
        #[serde(default = "default_hour")]
        hour_utc: u8,
        #[serde(default)]
        profiles: Vec<WindStation>,
        #[serde(default)]
        source: String,
    },
}

fn default_hour() -> u8 {
    12
}

impl Default for WindSpec {
    fn default() -> Self {
        Self::Off
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindStation {
    pub lat: f64,
    pub lon: f64,
    pub levels: Vec<WindLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindLevel {
    pub alt_m: f64,
    pub east_mps: f64,
    pub north_mps: f64,
}

impl WindSpec {
    pub fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }

    fn needs_fetch(&self) -> bool {
        match self {
            Self::Historical { profiles, .. } => profiles.is_empty(),
            _ => false,
        }
    }

    /// Settings only — site soundings stay off the mission-wide record.
    pub fn without_site_profiles(&self) -> Self {
        match self {
            Self::Historical {
                date,
                hour_utc,
                source,
                ..
            } => Self::Historical {
                date: date.clone(),
                hour_utc: *hour_utc,
                profiles: Vec::new(),
                source: source.clone(),
            },
            other => other.clone(),
        }
    }
}

/// Meteorological FROM direction → eastward / northward components.
pub fn meteo_to_enu(speed_mps: f64, from_deg: f64) -> (f64, f64) {
    let rad = from_deg.to_radians();
    (-speed_mps * rad.sin(), -speed_mps * rad.cos())
}

pub fn wind_enu(wind: &WindSpec, lat: f64, lon: f64, alt_m: f64) -> (f64, f64) {
    match wind {
        WindSpec::Off => (0.0, 0.0),
        WindSpec::Constant { speed_mps, from_deg } => {
            if *speed_mps <= 0.0 {
                (0.0, 0.0)
            } else {
                meteo_to_enu(*speed_mps, *from_deg)
            }
        }
        WindSpec::Historical { profiles, .. } => interpolate_stations(profiles, lat, lon, alt_m),
    }
}

pub fn wind_ecef(wind: &WindSpec, lat_deg: f64, lon_deg: f64, alt_m: f64) -> (f64, f64, f64) {
    let (ue, un) = wind_enu(wind, lat_deg, lon_deg, alt_m);
    if ue.abs() < 1e-12 && un.abs() < 1e-12 {
        return (0.0, 0.0, 0.0);
    }
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();
    let sl = lat.sin();
    let cl = lat.cos();
    let so = lon.sin();
    let co = lon.cos();
    (
        -so * ue + (-sl * co) * un,
        co * ue + (-sl * so) * un,
        cl * un,
    )
}

pub fn resolve_wind_spec(
    mut wind: WindSpec,
    launch_lat: f64,
    launch_lon: f64,
    aim_lat: f64,
    aim_lon: f64,
) -> Result<WindSpec, String> {
    validate_wind(&wind)?;
    if wind.needs_fetch() {
        wind = fetch_historical(&wind, launch_lat, launch_lon, aim_lat, aim_lon)?;
    }
    Ok(wind)
}

fn validate_wind(wind: &WindSpec) -> Result<(), String> {
    match wind {
        WindSpec::Off => Ok(()),
        WindSpec::Constant { speed_mps, from_deg } => {
            if !(0.0..=150.0).contains(speed_mps) {
                return Err("wind speed should be 0–150 m/s".into());
            }
            if !from_deg.is_finite() {
                return Err("wind direction is invalid".into());
            }
            Ok(())
        }
        WindSpec::Historical { date, hour_utc, .. } => {
            if date.len() != 10 || date.as_bytes()[4] != b'-' || date.as_bytes()[7] != b'-' {
                return Err("historical wind date must be YYYY-MM-DD".into());
            }
            if *hour_utc > 23 {
                return Err("wind hour must be 0–23 UTC".into());
            }
            Ok(())
        }
    }
}

fn interpolate_stations(stations: &[WindStation], lat: f64, lon: f64, alt_m: f64) -> (f64, f64) {
    if stations.is_empty() {
        return (0.0, 0.0);
    }
    if stations.len() == 1 {
        return sample_station(&stations[0], alt_m);
    }
    let mut wsum = 0.0;
    let mut east = 0.0;
    let mut north = 0.0;
    for station in stations {
        let dist = haversine_m(lat, lon, station.lat, station.lon).max(1_000.0);
        let w = 1.0 / (dist * dist);
        let (e, n) = sample_station(station, alt_m);
        east += w * e;
        north += w * n;
        wsum += w;
    }
    (east / wsum, north / wsum)
}

fn sample_station(station: &WindStation, alt_m: f64) -> (f64, f64) {
    let levels = &station.levels;
    if levels.is_empty() {
        return (0.0, 0.0);
    }
    if alt_m <= levels[0].alt_m {
        return (levels[0].east_mps, levels[0].north_mps);
    }
    let last = levels.last().unwrap();
    if alt_m >= last.alt_m {
        let fade = ((alt_m - last.alt_m) / FADE_ABOVE_M).clamp(0.0, 1.0);
        let s = 1.0 - fade;
        return (last.east_mps * s, last.north_mps * s);
    }
    for pair in levels.windows(2) {
        if alt_m <= pair[1].alt_m {
            let span = (pair[1].alt_m - pair[0].alt_m).max(1.0);
            let t = ((alt_m - pair[0].alt_m) / span).clamp(0.0, 1.0);
            return (
                pair[0].east_mps + t * (pair[1].east_mps - pair[0].east_mps),
                pair[0].north_mps + t * (pair[1].north_mps - pair[0].north_mps),
            );
        }
    }
    (last.east_mps, last.north_mps)
}

fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0;
    let p1 = lat1.to_radians();
    let p2 = lat2.to_radians();
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp * 0.5).sin().powi(2) + p1.cos() * p2.cos() * (dl * 0.5).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

fn geographic_mid(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> (f64, f64) {
    let mut dlon = lon2 - lon1;
    if dlon > 180.0 {
        dlon -= 360.0;
    } else if dlon < -180.0 {
        dlon += 360.0;
    }
    let mut lon = lon1 + 0.5 * dlon;
    if lon > 180.0 {
        lon -= 360.0;
    } else if lon < -180.0 {
        lon += 360.0;
    }
    ((lat1 + lat2) * 0.5, lon)
}

const SURFACE: &[(&str, &str, f64)] = &[
    ("wind_speed_10m", "wind_direction_10m", 10.0),
    ("wind_speed_100m", "wind_direction_100m", 100.0),
];

const PRESSURE_HPA: &[i32] = &[950, 925, 900, 850, 800, 700, 600, 500, 400, 300, 250, 200, 150, 100, 70, 50, 30];
const PRESSURE_ISA_M: &[f64] = &[
    500.0, 762.0, 988.0, 1_457.0, 1_948.0, 3_012.0, 4_206.0, 5_574.0, 7_185.0, 9_164.0, 10_363.0,
    11_784.0, 13_608.0, 16_180.0, 18_442.0, 20_576.0, 23_849.0,
];

fn hourly_params() -> String {
    let mut names = Vec::new();
    for (sp, dir, _) in SURFACE {
        names.push((*sp).to_string());
        names.push((*dir).to_string());
    }
    for hpa in PRESSURE_HPA {
        names.push(format!("wind_speed_{hpa}hPa"));
        names.push(format!("wind_direction_{hpa}hPa"));
        names.push(format!("geopotential_height_{hpa}hPa"));
    }
    names.join(",")
}

fn fetch_historical(
    wind: &WindSpec,
    launch_lat: f64,
    launch_lon: f64,
    aim_lat: f64,
    aim_lon: f64,
) -> Result<WindSpec, String> {
    let WindSpec::Historical { date, hour_utc, .. } = wind else {
        return Ok(wind.clone());
    };
    if date.as_str() < "2021-03-23" {
        return Err("historical pressure-level winds start 2021-03-23; pick a later date or use constant wind".into());
    }
    let mid = geographic_mid(launch_lat, launch_lon, aim_lat, aim_lon);
    let sites = [
        (launch_lat, launch_lon, "launch"),
        (mid.0, mid.1, "mid"),
        (aim_lat, aim_lon, "aim"),
    ];
    let mut profiles = Vec::new();
    let mut source_label = String::new();
    for (lat, lon, name) in sites {
        let (station, src) = fetch_station(lat, lon, date, *hour_utc, name)?;
        if station.levels.len() < 3 {
            return Err(format!("wind sounding at {name} had too few levels"));
        }
        if source_label.is_empty() {
            source_label = src;
        }
        profiles.push(station);
    }
    Ok(WindSpec::Historical {
        date: date.clone(),
        hour_utc: *hour_utc,
        profiles,
        source: source_label,
    })
}

fn fetch_station(
    lat: f64,
    lon: f64,
    date: &str,
    hour_utc: u8,
    name: &str,
) -> Result<(WindStation, String), String> {
    let params = hourly_params();
    let attempts = [
        (
            format!(
                "https://historical-forecast-api.open-meteo.com/v1/forecast?latitude={lat:.5}&longitude={lon:.5}&start_date={date}&end_date={date}&hourly={params}&wind_speed_unit=ms&timezone=GMT&models=gfs_global"
            ),
            format!("GFS {date} {hour_utc:02}Z"),
        ),
        (
            format!(
                "https://historical-forecast-api.open-meteo.com/v1/forecast?latitude={lat:.5}&longitude={lon:.5}&start_date={date}&end_date={date}&hourly={params}&wind_speed_unit=ms&timezone=GMT"
            ),
            format!("Open-Meteo {date} {hour_utc:02}Z"),
        ),
        (
            format!(
                "https://api.open-meteo.com/v1/gfs?latitude={lat:.5}&longitude={lon:.5}&start_date={date}&end_date={date}&hourly={params}&wind_speed_unit=ms&timezone=GMT"
            ),
            format!("GFS forecast {date} {hour_utc:02}Z"),
        ),
    ];
    let mut last_err = format!("no wind data at {name}");
    for (url, label) in attempts {
        match download_station(&url, lat, lon, hour_utc) {
            Ok(station) if station.levels.len() >= 3 => return Ok((station, label)),
            Ok(_) => last_err = format!("wind sounding at {name} was too thin"),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

fn download_station(url: &str, lat: f64, lon: f64, hour_utc: u8) -> Result<WindStation, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(25))
        .build();
    let body = agent
        .get(url)
        .set("User-Agent", "fauxrrt-trajectory-generator")
        .call()
        .map_err(|e| format!("wind request failed: {e}"))?
        .into_string()
        .map_err(|e| format!("wind response: {e}"))?;
    parse_open_meteo(&body, lat, lon, hour_utc)
}

fn parse_open_meteo(body: &str, lat: f64, lon: f64, hour_utc: u8) -> Result<WindStation, String> {
    let json: Value = serde_json::from_str(body).map_err(|e| format!("wind JSON: {e}"))?;
    if json.get("error").and_then(Value::as_bool) == Some(true) {
        let reason = json.get("reason").and_then(Value::as_str).unwrap_or("Open-Meteo error");
        return Err(reason.to_string());
    }
    let hourly = json.get("hourly").ok_or("wind response missing hourly data")?;
    let times = hourly
        .get("time")
        .and_then(Value::as_array)
        .ok_or("wind response missing times")?;
    let idx = pick_hour(times, hour_utc).ok_or("no wind sample at that hour")?;
    let elev = json.get("elevation").and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
    let mut levels = Vec::new();
    for (speed_key, dir_key, agl) in SURFACE {
        if let Some(level) = read_level(hourly, idx, speed_key, dir_key, elev + *agl) {
            levels.push(level);
        }
    }
    for (hpa, isa) in PRESSURE_HPA.iter().zip(PRESSURE_ISA_M.iter()) {
        let speed_key = format!("wind_speed_{hpa}hPa");
        let dir_key = format!("wind_direction_{hpa}hPa");
        let geo_key = format!("geopotential_height_{hpa}hPa");
        let alt = hourly
            .get(&geo_key)
            .and_then(Value::as_array)
            .and_then(|a| a.get(idx))
            .and_then(Value::as_f64)
            .filter(|h| h.is_finite() && *h > 0.0)
            .unwrap_or(*isa);
        if let Some(level) = read_level(hourly, idx, &speed_key, &dir_key, alt) {
            levels.push(level);
        }
    }
    levels.sort_by(|a, b| a.alt_m.total_cmp(&b.alt_m));
    levels.dedup_by(|a, b| (a.alt_m - b.alt_m).abs() < 5.0);
    Ok(WindStation { lat, lon, levels })
}

fn pick_hour(times: &[Value], hour_utc: u8) -> Option<usize> {
    let needle = format!("T{hour_utc:02}:");
    times.iter().position(|t| t.as_str().is_some_and(|s| s.contains(&needle)))
}

fn read_level(hourly: &Value, idx: usize, speed_key: &str, dir_key: &str, alt_m: f64) -> Option<WindLevel> {
    let speed = hourly.get(speed_key)?.as_array()?.get(idx)?.as_f64()?;
    let from_deg = hourly.get(dir_key)?.as_array()?.get(idx)?.as_f64()?;
    if !speed.is_finite() || !from_deg.is_finite() || speed < 0.0 {
        return None;
    }
    let (east_mps, north_mps) = meteo_to_enu(speed, from_deg);
    Some(WindLevel {
        alt_m,
        east_mps,
        north_mps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn west_wind_is_eastward() {
        let (e, n) = meteo_to_enu(10.0, 270.0);
        assert!((e - 10.0).abs() < 1e-9, "{e}");
        assert!(n.abs() < 1e-9, "{n}");
    }

    #[test]
    fn north_wind_is_southward() {
        let (e, n) = meteo_to_enu(8.0, 0.0);
        assert!(e.abs() < 1e-9);
        assert!((n + 8.0).abs() < 1e-9);
    }

    #[test]
    fn equator_eastward_wind_is_plus_y() {
        let wind = WindSpec::Constant {
            speed_mps: 12.0,
            from_deg: 270.0,
        };
        let (x, y, z) = wind_ecef(&wind, 0.0, 0.0, 1000.0);
        assert!(x.abs() < 1e-6, "{x}");
        assert!((y - 12.0).abs() < 1e-6, "{y}");
        assert!(z.abs() < 1e-6, "{z}");
    }

    #[test]
    fn profile_lerps_and_fades() {
        let station = WindStation {
            lat: 30.0,
            lon: -100.0,
            levels: vec![
                WindLevel {
                    alt_m: 0.0,
                    east_mps: 0.0,
                    north_mps: 10.0,
                },
                WindLevel {
                    alt_m: 10_000.0,
                    east_mps: 20.0,
                    north_mps: 10.0,
                },
            ],
        };
        let wind = WindSpec::Historical {
            date: "2024-01-01".into(),
            hour_utc: 12,
            profiles: vec![station],
            source: "test".into(),
        };
        let (e, n) = wind_enu(&wind, 30.0, -100.0, 5_000.0);
        assert!((e - 10.0).abs() < 1e-9);
        assert!((n - 10.0).abs() < 1e-9);
        let (e2, _) = wind_enu(&wind, 30.0, -100.0, 30_000.0);
        assert!(e2.abs() < 1e-9);
    }

    #[test]
    fn parses_open_meteo_hour() {
        let body = r#"{
            "elevation": 1200.0,
            "hourly": {
                "time": ["2024-06-01T00:00", "2024-06-01T12:00"],
                "wind_speed_10m": [2.0, 5.0],
                "wind_direction_10m": [270.0, 270.0],
                "wind_speed_100m": [3.0, 6.0],
                "wind_direction_100m": [270.0, 270.0],
                "wind_speed_850hPa": [null, 15.0],
                "wind_direction_850hPa": [null, 270.0],
                "geopotential_height_850hPa": [null, 1500.0]
            }
        }"#;
        let station = parse_open_meteo(body, 32.0, -106.0, 12).unwrap();
        assert!(station.levels.len() >= 3);
        assert!((station.levels[0].alt_m - 1210.0).abs() < 1e-9);
        let top = station.levels.last().unwrap();
        assert!((top.east_mps - 15.0).abs() < 1e-9);
        assert!((top.alt_m - 1500.0).abs() < 1e-9);
    }
}
