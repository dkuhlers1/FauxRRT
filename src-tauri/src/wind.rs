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
    /// Current GFS 10 m wind on the same 10°×15° grid as the globe surface-wind layer.
    /// East/north components are sampled by latitude and longitude at every height.
    Surface {
        #[serde(default)]
        time: String,
        #[serde(default)]
        source: String,
        #[serde(default)]
        lats: Vec<f64>,
        #[serde(default)]
        lons: Vec<f64>,
        #[serde(default)]
        east_mps: Vec<f64>,
        #[serde(default)]
        north_mps: Vec<f64>,
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
pub struct SurfaceStation {
    pub lat: f64,
    pub lon: f64,
    pub east_mps: f64,
    pub north_mps: f64,
}

/// What the globe particle layer should draw for the selected mission wind.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WindAnimationField {
    Off,
    Constant { speed_mps: f64, from_deg: f64 },
    Surface {
        time: String,
        source: String,
        lats: Vec<f64>,
        lons: Vec<f64>,
        east_mps: Vec<f64>,
        north_mps: Vec<f64>,
    },
    Historical {
        date: String,
        hour_utc: u8,
        source: String,
        regions: Vec<HistoricalAnimationRegion>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoricalAnimationRegion {
    pub stations: Vec<SurfaceStation>,
}

impl WindAnimationField {
    pub fn from_stored(wind: &WindSpec) -> Option<Self> {
        match wind {
            WindSpec::Off => Some(Self::Off),
            WindSpec::Constant { speed_mps, from_deg } => Some(Self::Constant {
                speed_mps: *speed_mps,
                from_deg: *from_deg,
            }),
            WindSpec::Surface {
                time,
                source,
                lats,
                lons,
                east_mps,
                north_mps,
            } if lats.len() >= 2
                && lons.len() >= 2
                && east_mps.len() == lats.len() * lons.len()
                && north_mps.len() == east_mps.len() =>
            {
                Some(Self::Surface {
                    time: time.clone(),
                    source: source.clone(),
                    lats: lats.clone(),
                    lons: lons.clone(),
                    east_mps: east_mps.clone(),
                    north_mps: north_mps.clone(),
                })
            }
            _ => None,
        }
    }
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
            Self::Surface { east_mps, .. } => east_mps.is_empty(),
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
            // The surface grid is the mission wind itself, not a site sounding.
            Self::Surface { .. } => self.clone(),
            other => other.clone(),
        }
    }
}

/// Meteorological FROM direction → eastward / northward components.
pub fn meteo_to_enu(speed_mps: f64, from_deg: f64) -> (f64, f64) {
    let rad = from_deg.to_radians();
    (-speed_mps * rad.sin(), -speed_mps * rad.cos())
}

/// Eastward / northward components → meteorological speed and FROM direction.
pub fn enu_to_meteo(east_mps: f64, north_mps: f64) -> (f64, f64) {
    let speed = east_mps.hypot(north_mps);
    if speed < 1e-9 {
        return (0.0, 0.0);
    }
    let mut from_deg = (-east_mps).atan2(-north_mps).to_degrees();
    if from_deg < 0.0 {
        from_deg += 360.0;
    }
    (speed, from_deg)
}

/// Mission wind for a new trajectory that has one site: a state sample or a 6DOF launch.
///
/// Off and constant wind are used as stored. Historical soundings and the Surface
/// GFS grid are fetched only when that selection has no profiles or grid yet.
/// Generated 3DOF flights resolve the same selection along launch and aim.
/// Launch plus a short aim offset. Historical globe streaks for a single
/// profile use this same pair, so they cover the sounding and not the globe.
pub fn profile_pair(lat: f64, lon: f64) -> (f64, f64, f64, f64) {
    let aim_lon = if lon <= 179.6 { lon + 0.4 } else { lon - 0.4 };
    (lat, lon, lat, aim_lon)
}

pub fn historical_resolve_key(date: &str, hour_utc: u8, launch_lat: f64, launch_lon: f64, aim_lat: f64, aim_lon: f64) -> String {
    format!("h:{date}:{hour_utc}:{launch_lat:.5}:{launch_lon:.5}:{aim_lat:.5}:{aim_lon:.5}")
}

pub fn prepare_flight_wind(wind: &WindSpec, lat: f64, lon: f64) -> Result<WindSpec, String> {
    let (launch_lat, launch_lon, aim_lat, aim_lon) = profile_pair(lat, lon);
    resolve_wind_spec(wind.clone(), launch_lat, launch_lon, aim_lat, aim_lon)
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
        WindSpec::Surface {
            lats,
            lons,
            east_mps,
            north_mps,
            ..
        } => sample_surface_grid(lats, lons, east_mps, north_mps, lat, lon),
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
        wind = match &wind {
            WindSpec::Historical { .. } => {
                fetch_historical(&wind, launch_lat, launch_lon, aim_lat, aim_lon)?
            }
            WindSpec::Surface { .. } => fetch_surface()?,
            other => other.clone(),
        };
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
        WindSpec::Surface {
            lats,
            lons,
            east_mps,
            north_mps,
            ..
        } => {
            if east_mps.is_empty() && north_mps.is_empty() && lats.is_empty() && lons.is_empty() {
                return Ok(());
            }
            if lats.len() < 2 || lons.len() < 2 || east_mps.len() != lats.len() * lons.len() || north_mps.len() != east_mps.len()
            {
                return Err("surface wind grid is incomplete".into());
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

/// Surface east/north of each historical station, using the same level
/// sample trajectories use at the ground.
pub fn surface_stations(wind: &WindSpec) -> Vec<SurfaceStation> {
    let WindSpec::Historical { profiles, .. } = wind else {
        return Vec::new();
    };
    profiles
        .iter()
        .map(|station| {
            let (east_mps, north_mps) = sample_station(station, 0.0);
            SurfaceStation {
                lat: station.lat,
                lon: station.lon,
                east_mps,
                north_mps,
            }
        })
        .collect()
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

fn wrap_lon(lon: f64) -> f64 {
    let x = ((lon + 180.0) % 360.0 + 360.0) % 360.0 - 180.0;
    if x == 0.0 { 0.0 } else { x }
}

fn sample_surface_grid(
    lats: &[f64],
    lons: &[f64],
    east: &[f64],
    north: &[f64],
    lat: f64,
    lon: f64,
) -> (f64, f64) {
    let ny = lats.len();
    let nx = lons.len();
    if ny < 2 || nx < 2 || east.len() != nx * ny || north.len() != east.len() {
        return (0.0, 0.0);
    }
    let dlat = lats[1] - lats[0];
    let dlon = lons[1] - lons[0];
    if dlat <= 0.0 || dlon <= 0.0 {
        return (0.0, 0.0);
    }
    let fy = ((lat - lats[0]) / dlat).clamp(0.0, (ny - 1) as f64);
    let j0 = (fy.floor() as usize).min(ny - 1);
    let j1 = (j0 + 1).min(ny - 1);
    let ty = fy - j0 as f64;
    let global = (nx as f64 * dlon - 360.0).abs() < 1e-3;
    let mut fx = (wrap_lon(lon) - lons[0]) / dlon;
    if global {
        let n = nx as f64;
        fx = ((fx % n) + n) % n;
    } else if fx < 0.0 {
        fx = 0.0;
    } else if fx > (nx - 1) as f64 {
        fx = (nx - 1) as f64;
    }
    let i0 = (fx.floor() as usize).min(nx - 1);
    let tx = fx - i0 as f64;
    let i1 = if i0 + 1 >= nx { 0 } else { i0 + 1 };
    let sy = 1.0 - ty;
    let sx = 1.0 - tx;
    let at = |j: usize, i: usize| j * nx + i;
    let blend = |values: &[f64]| {
        values[at(j0, i0)] * sx * sy
            + values[at(j0, i1)] * tx * sy
            + values[at(j1, i0)] * sx * ty
            + values[at(j1, i1)] * tx * ty
    };
    (blend(east), blend(north))
}

const GFS_SURFACE_LAT_STEP: usize = 10;
const GFS_SURFACE_LON_STEP: usize = 15;

fn fetch_surface() -> Result<WindSpec, String> {
    let lats = (-80..=80).step_by(GFS_SURFACE_LAT_STEP).map(|v| v as f64).collect::<Vec<_>>();
    let lons = (-180..180).step_by(GFS_SURFACE_LON_STEP).map(|v| v as f64).collect::<Vec<_>>();
    let mut points = Vec::with_capacity(lats.len() * lons.len());
    for lat in &lats {
        for lon in &lons {
            points.push((*lat, *lon));
        }
    }
    let mut east = vec![0.0; points.len()];
    let mut north = vec![0.0; points.len()];
    let mut time = String::new();
    let mut missing = 0usize;
    let mut offset = 0;
    while offset < points.len() {
        let end = (offset + 80).min(points.len());
        let batch = &points[offset..end];
        let rows = fetch_surface_batch(batch)?;
        if rows.len() != batch.len() {
            return Err("GFS surface wind batch size mismatch".into());
        }
        for (i, row) in rows.into_iter().enumerate() {
            match row {
                Some((e, n, stamp)) => {
                    east[offset + i] = e;
                    north[offset + i] = n;
                    if time.is_empty() {
                        time = stamp;
                    }
                }
                None => missing += 1,
            }
        }
        offset = end;
    }
    if missing * 4 > points.len() {
        return Err("GFS response was missing most of the 10 m wind grid".into());
    }
    Ok(WindSpec::Surface {
        time,
        source: "Open-Meteo GFS 10 m".into(),
        lats,
        lons,
        east_mps: east,
        north_mps: north,
    })
}

fn fetch_surface_batch(points: &[(f64, f64)]) -> Result<Vec<Option<(f64, f64, String)>>, String> {
    let lats = points.iter().map(|(lat, _)| format!("{lat:.4}")).collect::<Vec<_>>().join(",");
    let lons = points.iter().map(|(_, lon)| format!("{lon:.4}")).collect::<Vec<_>>().join(",");
    let url = format!(
        "https://api.open-meteo.com/v1/gfs?latitude={lats}&longitude={lons}&current=wind_speed_10m,wind_direction_10m&wind_speed_unit=ms&timezone=GMT"
    );
    let body = http_get_retry(&url)?;
    parse_gfs_current_batch(&body)
}

fn http_get_retry(url: &str) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build();
    let mut last_err = "wind request failed".to_string();
    for attempt in 0u32..6 {
        match agent.get(url).set("User-Agent", "fauxrrt-trajectory-generator").call() {
            Ok(resp) => {
                return resp.into_string().map_err(|e| format!("wind response: {e}"));
            }
            Err(ureq::Error::Status(code, resp)) if attempt < 1 => {
                let body = resp.into_string().unwrap_or_default();
                let lower = body.to_lowercase();
                if code == 429 || code >= 500 || lower.contains("limit") || lower.contains("overload") {
                    last_err = "GFS wind request was rate limited".into();
                    let pause = 800u64.saturating_mul(u64::from(attempt) + 1);
                    std::thread::sleep(std::time::Duration::from_millis(pause));
                    continue;
                }
                let reason = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|json| json.get("reason").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_else(|| format!("GFS wind request failed ({code})"));
                return Err(reason);
            }
            Err(err) => return Err(format!("wind request failed: {err}")),
        }
    }
    Err(last_err)
}

fn parse_gfs_current_batch(body: &str) -> Result<Vec<Option<(f64, f64, String)>>, String> {
    let json: Value = serde_json::from_str(body).map_err(|e| format!("wind JSON: {e}"))?;
    if json.get("error").and_then(Value::as_bool) == Some(true) {
        let reason = json.get("reason").and_then(Value::as_str).unwrap_or("Open-Meteo error");
        return Err(reason.to_string());
    }
    let rows = json.as_array().ok_or("GFS wind response was not a location grid")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let current = row.get("current");
        let speed = current.and_then(|c| c.get("wind_speed_10m")).and_then(Value::as_f64);
        let direction = current.and_then(|c| c.get("wind_direction_10m")).and_then(Value::as_f64);
        let stamp = current
            .and_then(|c| c.get("time"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match (speed, direction) {
            (Some(speed), Some(direction)) if speed.is_finite() && direction.is_finite() && speed >= 0.0 => {
                let (east, north) = meteo_to_enu(speed, direction);
                out.push(Some((east, north, stamp)));
            }
            _ => out.push(None),
        }
    }
    Ok(out)
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

    #[test]
    fn surface_grid_is_the_mission_wind_field() {
        let wind = WindSpec::Surface {
            time: "2026-10-07T00:00".into(),
            source: "Open-Meteo GFS 10 m".into(),
            lats: vec![0.0, 10.0],
            lons: vec![-10.0, 0.0],
            east_mps: vec![10.0, 0.0, 0.0, 0.0],
            north_mps: vec![0.0, 0.0, 0.0, 0.0],
        };
        let (e, n) = wind_enu(&wind, 0.0, -10.0, 40_000.0);
        assert!((e - 10.0).abs() < 1e-6, "{e}");
        assert!(n.abs() < 1e-6, "{n}");
        let (mid, _) = wind_enu(&wind, 5.0, -5.0, 1_000.0);
        assert!((mid - 2.5).abs() < 1e-6, "{mid}");
        let kept = wind.without_site_profiles();
        assert!(!kept.needs_fetch());
        let (e2, _) = wind_enu(&kept, 0.0, -10.0, 0.0);
        assert!((e2 - 10.0).abs() < 1e-6);
    }

    #[test]
    fn surface_grid_wraps_longitude() {
        let wind = WindSpec::Surface {
            time: String::new(),
            source: String::new(),
            lats: vec![0.0, 10.0],
            lons: vec![-180.0, 0.0],
            east_mps: vec![0.0, 10.0, 0.0, 0.0],
            north_mps: vec![0.0, 0.0, 0.0, 0.0],
        };
        let (e, _) = wind_enu(&wind, 0.0, 90.0, 0.0);
        assert!((e - 5.0).abs() < 1e-6, "{e}");
    }

    #[test]
    fn parses_gfs_current_batch() {
        let body = r#"[{
            "current": {"time": "2026-10-07T00:00", "wind_speed_10m": 10.0, "wind_direction_10m": 270}
        }, {
            "current": {"time": "2026-10-07T00:00", "wind_speed_10m": null, "wind_direction_10m": null}
        }]"#;
        let rows = parse_gfs_current_batch(body).unwrap();
        assert_eq!(rows.len(), 2);
        let (e, n, stamp) = rows[0].clone().unwrap();
        assert!((e - 10.0).abs() < 1e-9);
        assert!(n.abs() < 1e-9);
        assert_eq!(stamp, "2026-10-07T00:00");
        assert!(rows[1].is_none());
    }

    #[test]
    fn enu_round_trips_a_west_wind() {
        let (east, north) = meteo_to_enu(12.5, 270.0);
        let (speed, from) = enu_to_meteo(east, north);
        assert!((speed - 12.5).abs() < 1e-9, "{speed}");
        assert!((from - 270.0).abs() < 1e-9, "{from}");
    }

    #[test]
    fn prepare_flight_wind_keeps_the_selected_field() {
        let historical = WindSpec::Historical {
            date: "2024-06-01".into(),
            hour_utc: 12,
            source: "test".into(),
            profiles: vec![WindStation {
                lat: 32.4,
                lon: -106.4,
                levels: vec![
                    WindLevel {
                        alt_m: 0.0,
                        east_mps: 8.0,
                        north_mps: 0.0,
                    },
                    WindLevel {
                        alt_m: 10_000.0,
                        east_mps: 20.0,
                        north_mps: 0.0,
                    },
                ],
            }],
        };
        let prepared = prepare_flight_wind(&historical, 32.4, -106.4).unwrap();
        let (east, north) = wind_enu(&prepared, 32.4, -106.4, 5_000.0);
        assert!((east - 14.0).abs() < 1e-6, "{east}");
        assert!(north.abs() < 1e-6);
        let stripped = historical.without_site_profiles();
        assert!(stripped.needs_fetch());
        let (zero_e, zero_n) = wind_enu(&stripped, 32.4, -106.4, 5_000.0);
        assert!(zero_e.abs() < 1e-9 && zero_n.abs() < 1e-9);

        let surface = WindSpec::Surface {
            time: "2026-10-08T00:00".into(),
            source: "test".into(),
            lats: vec![30.0, 40.0],
            lons: vec![-110.0, -100.0],
            east_mps: vec![16.0, 16.0, 16.0, 16.0],
            north_mps: vec![0.0, 0.0, 0.0, 0.0],
        };
        let prepared = prepare_flight_wind(&surface, 32.4, -106.4).unwrap();
        let (east, _) = wind_enu(&prepared, 32.4, -106.4, 40_000.0);
        assert!((east - 16.0).abs() < 1e-6, "{east}");
    }

    #[test]
    fn profile_pair_is_a_short_local_offset() {
        let (lat, lon, aim_lat, aim_lon) = profile_pair(32.0, -106.0);
        assert!((lat - 32.0).abs() < 1e-12);
        assert!((lon + 106.0).abs() < 1e-12);
        assert!((aim_lat - 32.0).abs() < 1e-12);
        assert!((aim_lon + 105.6).abs() < 1e-12);
        let (_, _, _, wrapped) = profile_pair(10.0, 179.8);
        assert!((wrapped - 179.4).abs() < 1e-12);
    }

    #[test]
    fn surface_stations_use_the_ground_level_of_the_profile() {
        let wind = WindSpec::Historical {
            date: "2024-06-01".into(),
            hour_utc: 12,
            source: "test".into(),
            profiles: vec![WindStation {
                lat: 32.4,
                lon: -106.4,
                levels: vec![
                    WindLevel { alt_m: 10.0, east_mps: 8.0, north_mps: 1.0 },
                    WindLevel { alt_m: 10_000.0, east_mps: 40.0, north_mps: -5.0 },
                ],
            }],
        };
        let stations = surface_stations(&wind);
        assert_eq!(stations.len(), 1);
        assert!((stations[0].lat - 32.4).abs() < 1e-12);
        assert!((stations[0].lon + 106.4).abs() < 1e-12);
        assert!((stations[0].east_mps - 8.0).abs() < 1e-12);
        assert!((stations[0].north_mps - 1.0).abs() < 1e-12);
        assert!(surface_stations(&WindSpec::Off).is_empty());
    }
}
