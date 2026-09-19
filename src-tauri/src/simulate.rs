//! Sample a state from an existing trajectory, then propagate a spent stage,
//! FTS debris, or a max-g nav-failure turn followed by FTS.

use serde::{Deserialize, Serialize};

use crate::generate::{ecef_coast_accel, enu_basis, integrate_turn, track_from_state, Vec3};
use crate::geodesy::{ecef_to_lla, lla_to_ecef};
use crate::parse::ParsedTrack;
use crate::wind::WindSpec;
use rayon::prelude::*;

const MAX_FRAGMENTS: usize = 2_000;
pub const MIN_BREAKUP_ALT_M: f64 = 80.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebrisPiece {
    pub name: String,
    pub ballistic_coeff: f64,
    /// Impulsive Δv magnitude applied at breakup. Direction is sampled uniformly.
    pub delta_v_mps: f64,
    #[serde(default = "default_count")]
    pub count: u32,
}

fn default_count() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebrisCatalog {
    pub id: u64,
    pub name: String,
    pub pieces: Vec<DebrisPiece>,
}

impl DebrisCatalog {
    pub fn default_fts() -> Self {
        Self {
            id: 1,
            name: "Default FTS".into(),
            pieces: vec![
                piece("Propellant tank", 800.0, 40.0, 2),
                piece("Aft skirt", 350.0, 60.0, 2),
                piece("Avionics", 180.0, 80.0, 2),
                piece("Skin panel", 70.0, 100.0, 6),
                piece("Fragment", 20.0, 140.0, 10),
            ],
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("catalogue needs a name".into());
        }
        if self.pieces.is_empty() {
            return Err("catalogue has no pieces".into());
        }
        let mut total = 0usize;
        for piece in &self.pieces {
            if piece.name.trim().is_empty() {
                return Err("each piece needs a name".into());
            }
            if !(5.0..=50_000.0).contains(&piece.ballistic_coeff) {
                return Err(format!(
                    "{}: ballistic coefficient should be 5–50000 kg/m²",
                    piece.name
                ));
            }
            if !(0.0..=2_000.0).contains(&piece.delta_v_mps) {
                return Err(format!("{}: Δv should be 0–2000 m/s", piece.name));
            }
            if piece.count == 0 || piece.count > 200 {
                return Err(format!("{}: count should be 1–200", piece.name));
            }
            total += piece.count as usize;
        }
        if total > MAX_FRAGMENTS {
            return Err(format!("catalogue would spawn {total} fragments (max {MAX_FRAGMENTS})"));
        }
        Ok(())
    }

    pub fn fragment_count(&self) -> usize {
        self.pieces.iter().map(|p| p.count as usize).sum()
    }
}

fn piece(name: &str, ballistic_coeff: f64, delta_v_mps: f64, count: u32) -> DebrisPiece {
    DebrisPiece {
        name: name.into(),
        ballistic_coeff,
        delta_v_mps,
        count,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSample {
    pub track_id: u64,
    pub time_s: f64,
    pub time_start: f64,
    pub time_end: f64,
    pub has_clock: bool,
    pub lon: f64,
    pub lat: f64,
    pub alt_m: f64,
    pub vx_ecef: f64,
    pub vy_ecef: f64,
    pub vz_ecef: f64,
    pub speed_mps: f64,
    pub heading_deg: f64,
    pub flight_path_deg: f64,
}

impl StateSample {
    pub fn position(&self) -> Vec3 {
        Vec3::from_lla(self.lat, self.lon, self.alt_m)
    }

    pub fn velocity(&self) -> Vec3 {
        Vec3::from_ecef(self.vx_ecef, self.vy_ecef, self.vz_ecef)
    }
}

/// Snapshot used to regenerate a simulated track when mission wind changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulateOrigin {
    pub r_ecef: [f64; 3],
    pub v_ecef: [f64; 3],
    pub ballistic_coeff: f64,
    pub time_offset: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_track_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_time_s: Option<f64>,
    /// Impulsive Δv added at breakup. Used when re-sampling the parent after wind changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_v_ecef: Option<[f64; 3]>,
    /// HAE floor matching the parent landing (pad), not necessarily the ellipsoid.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub ground_alt_m: f64,
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOrigin {
    pub duration_s: f64,
    pub max_g: f64,
    pub side: f64,
    #[serde(default = "default_true")]
    pub sustain_speed: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TurnSide {
    Left,
    Right,
    Both,
}

impl TurnSide {
    fn sides(self) -> Vec<(f64, &'static str)> {
        match self {
            TurnSide::Left => vec![(1.0, "L")],
            TurnSide::Right => vec![(-1.0, "R")],
            TurnSide::Both => vec![(1.0, "L"), (-1.0, "R")],
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TimeDist {
    #[default]
    Point,
    Uniform,
    Normal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpentStageSpec {
    pub source_track_id: u64,
    pub time_s: f64,
    pub ballistic_coeff: f64,
    #[serde(default)]
    pub object_id: Option<u64>,
    #[serde(default)]
    pub object_name: Option<String>,
    pub mode_name: String,
    #[serde(default)]
    pub dist: TimeDist,
    #[serde(default = "default_stage_count")]
    pub count: u32,
    #[serde(default)]
    pub t_min: Option<f64>,
    #[serde(default)]
    pub t_max: Option<f64>,
    #[serde(default)]
    pub sigma_s: Option<f64>,
    /// For `Normal`, keep samples inside μ ± n_sigma·σ (then the track span).
    #[serde(default = "default_n_sigma")]
    pub n_sigma: f64,
    #[serde(default = "default_seed")]
    pub seed: u64,
}

fn default_stage_count() -> u32 {
    1
}

fn default_n_sigma() -> f64 {
    3.0
}

impl Default for SpentStageSpec {
    fn default() -> Self {
        Self {
            source_track_id: 0,
            time_s: 0.0,
            ballistic_coeff: 150.0,
            object_id: None,
            object_name: None,
            mode_name: "Staging".into(),
            dist: TimeDist::Point,
            count: 1,
            t_min: None,
            t_max: None,
            sigma_s: None,
            n_sigma: default_n_sigma(),
            seed: 1,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FtsSpec {
    pub source_track_id: u64,
    pub time_s: f64,
    pub catalog_id: u64,
    pub object_id: Option<u64>,
    pub object_name: Option<String>,
    pub mode_name: String,
    #[serde(default = "default_seed")]
    pub seed: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NavFailSpec {
    pub source_track_id: u64,
    pub times_s: Vec<f64>,
    pub catalog_id: u64,
    pub object_id: Option<u64>,
    pub object_name: Option<String>,
    pub mode_name: String,
    #[serde(default = "default_max_g")]
    pub max_g: f64,
    #[serde(default = "default_turn_s")]
    pub turn_duration_s: f64,
    #[serde(default)]
    pub turn_side: TurnSide,
    #[serde(default = "default_true")]
    pub sustain_speed: bool,
    #[serde(default = "default_seed")]
    pub seed: u64,
}

impl Default for TurnSide {
    fn default() -> Self {
        TurnSide::Both
    }
}

fn default_seed() -> u64 {
    1
}

fn default_max_g() -> f64 {
    5.0
}

fn default_turn_s() -> f64 {
    5.0
}

pub struct BuiltTrack {
    pub name: String,
    pub parsed: ParsedTrack,
    pub origin: SimulateOrigin,
    pub weight: f64,
}

pub fn sample_track_state(
    track_id: u64,
    lla: &[f32],
    times: Option<&[f64]>,
    time_s: f64,
) -> Result<StateSample, String> {
    let n = lla.len() / 3;
    if n < 2 {
        return Err("track needs at least two states".into());
    }
    let has_clock = times.map(|t| t.len() == n).unwrap_or(false);
    let clock: Vec<f64> = if has_clock {
        times.unwrap().to_vec()
    } else {
        (0..n).map(|i| i as f64).collect()
    };
    let time_start = clock[0];
    let time_end = *clock.last().unwrap();
    if !time_start.is_finite() || !time_end.is_finite() || time_end < time_start {
        return Err("track time span is invalid".into());
    }
    let t = time_s.clamp(time_start, time_end);
    let mut i = 0usize;
    for k in 0..n - 1 {
        if clock[k + 1] >= t {
            i = k;
            break;
        }
        i = k;
    }
    let dt = clock[i + 1] - clock[i];
    let frac = if dt.abs() < 1e-12 {
        0.0
    } else {
        ((t - clock[i]) / dt).clamp(0.0, 1.0)
    };
    let r0 = ecef_of(&lla[i * 3..i * 3 + 3]);
    let r1 = ecef_of(&lla[(i + 1) * 3..(i + 1) * 3 + 3]);
    let r = r0.add(r1.sub(r0).scale(frac));
    let (lon, lat, alt) = ecef_to_lla(r.x, r.y, r.z);
    let v = velocity_on_segment(lla, &clock, i, frac);
    Ok(finish_sample(
        track_id, t, time_start, time_end, has_clock, lon, lat, alt, v,
    ))
}

fn velocity_on_segment(lla: &[f32], clock: &[f64], i: usize, frac: f64) -> Vec3 {
    let n = lla.len() / 3;
    let mut j0 = i;
    let mut j1 = (i + 1).min(n - 1);
    while j1 + 1 < n && (clock[j1] - clock[j0]).abs() < 0.2 {
        j1 += 1;
    }
    while j0 > 0 && (clock[j1] - clock[j0]).abs() < 0.2 {
        j0 -= 1;
    }
    let span = clock[j1] - clock[j0];
    if span.abs() < 1e-8 {
        return Vec3::new(0.0, 0.0, 0.0);
    }
    let a0 = ecef_of(&lla[j0 * 3..j0 * 3 + 3]);
    let a1 = ecef_of(&lla[j1 * 3..j1 * 3 + 3]);
    let v_avg = a1.sub(a0).scale(1.0 / span);
    let r_mid = a0.add(a1.sub(a0).scale(0.5));
    let a = ecef_coast_accel(r_mid, v_avg);
    let t_mid = 0.5 * (clock[j0] + clock[j1]);
    let t = clock[i] + frac * (clock[i + 1] - clock[i]);
    v_avg.add(a.scale(t - t_mid))
}

/// HAE to stop coasts. Use the parent's landing if it reached the ground;
/// otherwise the ellipsoid so clipped mid-air tracks still fall to Earth.
pub fn ground_floor_alt(lla: &[f32]) -> f64 {
    let n = lla.len() / 3;
    if n == 0 {
        return 0.0;
    }
    let mut min_a = f32::INFINITY;
    let mut max_a = f32::NEG_INFINITY;
    for i in 0..n {
        let a = lla[i * 3 + 2];
        min_a = min_a.min(a);
        max_a = max_a.max(a);
    }
    if !min_a.is_finite() {
        return 0.0;
    }
    let min_a = min_a as f64;
    let max_a = max_a as f64;
    let last = lla[(n - 1) * 3 + 2] as f64;
    let landed = (max_a - min_a) > 80.0 && (last - min_a).abs() < 80.0;
    if landed || min_a < 5_000.0 {
        min_a.max(-50.0)
    } else {
        0.0
    }
}

fn finish_sample(
    track_id: u64,
    time_s: f64,
    time_start: f64,
    time_end: f64,
    has_clock: bool,
    lon: f64,
    lat: f64,
    alt_m: f64,
    v: Vec3,
) -> StateSample {
    let (east, north, up) = enu_basis(lat, lon);
    let ve = v.dot(east);
    let vn = v.dot(north);
    let vu = v.dot(up);
    let horiz = ve.hypot(vn);
    StateSample {
        track_id,
        time_s,
        time_start,
        time_end,
        has_clock,
        lon,
        lat,
        alt_m,
        vx_ecef: v.x,
        vy_ecef: v.y,
        vz_ecef: v.z,
        speed_mps: v.norm(),
        heading_deg: ve.atan2(vn).to_degrees(),
        flight_path_deg: vu.atan2(horiz).to_degrees(),
    }
}

fn ecef_of(lla: &[f32]) -> Vec3 {
    let (x, y, z) = lla_to_ecef(lla[1] as f64, lla[0] as f64, lla[2] as f64);
    Vec3::from_ecef(x, y, z)
}

const MAX_STAGE_SAMPLES: u32 = 200;

pub fn sample_stage_times(
    spec: &SpentStageSpec,
    track_start: f64,
    track_end: f64,
) -> Result<Vec<f64>, String> {
    let lo = track_start.min(track_end);
    let hi = track_start.max(track_end);
    if !lo.is_finite() || !hi.is_finite() || hi < lo {
        return Err("source trajectory time span is invalid".into());
    }
    let clamp_t = |t: f64| t.clamp(lo, hi);
    match spec.dist {
        TimeDist::Point => Ok(vec![clamp_t(spec.time_s)]),
        TimeDist::Uniform | TimeDist::Normal => {
            let count = spec.count;
            if count == 0 || count > MAX_STAGE_SAMPLES {
                return Err(format!(
                    "separation samples should be 1–{MAX_STAGE_SAMPLES}"
                ));
            }
            let mut t_min = clamp_t(spec.t_min.unwrap_or(lo));
            let mut t_max = clamp_t(spec.t_max.unwrap_or(hi));
            if t_max < t_min {
                std::mem::swap(&mut t_min, &mut t_max);
            }
            if (t_max - t_min).abs() < 1e-9 {
                return Ok(vec![t_min; count as usize]);
            }
            if spec.dist == TimeDist::Uniform {
                return Ok(sample_uniform_times(t_min, t_max, count, spec.seed));
            }
            let mean = clamp_t(spec.time_s);
            let sigma = spec.sigma_s.unwrap_or(1.0);
            if !(1e-6..=1.0e6).contains(&sigma) {
                return Err("σ should be a positive number of seconds".into());
            }
            let n_sigma = if spec.n_sigma.is_finite() && spec.n_sigma > 0.0 {
                spec.n_sigma.clamp(0.1, 10.0)
            } else {
                default_n_sigma()
            };
            let t_min = clamp_t(mean - n_sigma * sigma);
            let t_max = clamp_t(mean + n_sigma * sigma);
            if t_max < t_min || (t_max - t_min).abs() < 1e-9 {
                return Ok(vec![mean; count as usize]);
            }
            Ok(sample_normal_times(mean, sigma, t_min, t_max, count, spec.seed))
        }
    }
}

fn sample_uniform_times(t_min: f64, t_max: f64, count: u32, seed: u64) -> Vec<f64> {
    let mut rng = Rng::new(seed.max(1));
    let span = t_max - t_min;
    let n = count as f64;
    (0..count)
        .map(|i| {
            let u = (i as f64 + rng.unit()) / n;
            t_min + u.clamp(0.0, 1.0) * span
        })
        .collect()
}

fn sample_normal_times(
    mean: f64,
    sigma: f64,
    t_min: f64,
    t_max: f64,
    count: u32,
    seed: u64,
) -> Vec<f64> {
    let mut rng = Rng::new(seed.max(1));
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut t = mean;
        for _ in 0..64 {
            t = mean + sigma * rng.gauss();
            if t >= t_min && t <= t_max {
                break;
            }
        }
        out.push(t.clamp(t_min, t_max));
    }
    out
}

pub fn build_spent_stage(
    sample: &StateSample,
    spec: &SpentStageSpec,
    wind: &WindSpec,
    ground_alt_m: f64,
) -> Result<BuiltTrack, String> {
    if !(5.0..=50_000.0).contains(&spec.ballistic_coeff) {
        return Err("ballistic coefficient should be 5–50000 kg/m²".into());
    }
    if sample.alt_m < 20.0 {
        return Err("state is already on the ground".into());
    }
    let r = sample.position();
    let v = sample.velocity();
    let parsed = track_from_state(r, v, spec.ballistic_coeff, wind, ground_alt_m, sample.time_s)?;
    Ok(BuiltTrack {
        name: format!("Stage t{:.2}", sample.time_s),
        parsed,
        origin: SimulateOrigin {
            r_ecef: r.to_array(),
            v_ecef: v.to_array(),
            ballistic_coeff: spec.ballistic_coeff,
            time_offset: sample.time_s,
            turn: None,
            source_track_id: Some(sample.track_id),
            source_time_s: Some(sample.time_s),
            delta_v_ecef: None,
            ground_alt_m,
        },
        weight: 1.0,
    })
}

pub fn build_fts_debris(
    sample: &StateSample,
    catalog: &DebrisCatalog,
    seed: u64,
    wind: &WindSpec,
    name_prefix: &str,
    ground_alt_m: f64,
) -> Result<Vec<BuiltTrack>, String> {
    catalog.validate()?;
    if sample.alt_m < 20.0 {
        return Err("state is already on the ground".into());
    }
    spawn_debris(
        sample.position(),
        sample.velocity(),
        sample.time_s,
        catalog,
        seed,
        wind,
        name_prefix,
        sample,
        None,
        ground_alt_m,
    )
}

pub fn build_nav_failure(
    sample: &StateSample,
    spec: &NavFailSpec,
    catalog: &DebrisCatalog,
    wind: &WindSpec,
    ground_alt_m: f64,
) -> Result<Vec<BuiltTrack>, String> {
    catalog.validate()?;
    if !(0.1..=20.0).contains(&spec.max_g) {
        return Err("max g should be 0.1–20".into());
    }
    if !(0.1..=120.0).contains(&spec.turn_duration_s) {
        return Err("turn duration should be 0.1–120 s".into());
    }
    let sides = spec.turn_side.sides();
    let events = spec.times_s.len().max(1);
    let total = events * sides.len() * catalog.fragment_count();
    if total > MAX_FRAGMENTS {
        return Err(format!(
            "nav + FTS would spawn {total} fragments (max {MAX_FRAGMENTS}). Use fewer times or pieces."
        ));
    }
    let mut out = Vec::with_capacity(total + events * sides.len());
    let mut rng_seed = spec.seed;
    for side in &sides {
        let (parsed_turn, r_fts, v_fts) = integrate_turn(
            sample.position(),
            sample.velocity(),
            2_500.0,
            wind,
            spec.turn_duration_s,
            spec.max_g,
            side.0,
            spec.sustain_speed,
            sample.time_s,
        )?;
        out.push(BuiltTrack {
            name: format!("Turn t{:.0}-{}", sample.time_s, side.1),
            parsed: parsed_turn,
            origin: SimulateOrigin {
                r_ecef: sample.position().to_array(),
                v_ecef: sample.velocity().to_array(),
                ballistic_coeff: 2_500.0,
                time_offset: sample.time_s,
                turn: Some(TurnOrigin {
                    duration_s: spec.turn_duration_s,
                    max_g: spec.max_g,
                    side: side.0,
                    sustain_speed: spec.sustain_speed,
                }),
                source_track_id: Some(sample.track_id),
                source_time_s: Some(sample.time_s),
                delta_v_ecef: None,
                ground_alt_m,
            },
            weight: 0.0,
        });
        let prefix = format!("t{:.0}-{}", sample.time_s, side.1);
        let fts_time = sample.time_s + spec.turn_duration_s;
        let turn = TurnOrigin {
            duration_s: spec.turn_duration_s,
            max_g: spec.max_g,
            side: side.0,
            sustain_speed: spec.sustain_speed,
        };
        let mut debris = spawn_debris(
            r_fts,
            v_fts,
            fts_time,
            catalog,
            rng_seed,
            wind,
            &prefix,
            sample,
            Some(turn),
            ground_alt_m,
        )?;
        out.append(&mut debris);
        rng_seed = rng_seed.wrapping_add(10_007);
    }
    Ok(out)
}

pub fn regenerate_simulated(
    origin: &SimulateOrigin,
    wind: &WindSpec,
    resampled: Option<&StateSample>,
) -> Result<ParsedTrack, String> {
    if let Some(sample) = resampled {
        return regenerate_from_sample(origin, wind, sample);
    }
    let r = Vec3::from_ecef(origin.r_ecef[0], origin.r_ecef[1], origin.r_ecef[2]);
    let v = Vec3::from_ecef(origin.v_ecef[0], origin.v_ecef[1], origin.v_ecef[2]);
    if let Some(turn) = &origin.turn {
        if origin.delta_v_ecef.is_none() {
            let (track, _, _) = integrate_turn(
                r,
                v,
                origin.ballistic_coeff,
                wind,
                turn.duration_s,
                turn.max_g,
                turn.side,
                turn.sustain_speed,
                origin.time_offset,
            )?;
            return Ok(track);
        }
    }
    track_from_state(r, v, origin.ballistic_coeff, wind, origin.ground_alt_m, origin.time_offset)
}

fn regenerate_from_sample(
    origin: &SimulateOrigin,
    wind: &WindSpec,
    sample: &StateSample,
) -> Result<ParsedTrack, String> {
    let mut r = sample.position();
    let mut v = sample.velocity();
    let mut t0 = sample.time_s;
    if let Some(turn) = &origin.turn {
        let (track, r1, v1) = integrate_turn(
            r,
            v,
            origin.ballistic_coeff.max(5.0),
            wind,
            turn.duration_s,
            turn.max_g,
            turn.side,
            turn.sustain_speed,
            t0,
        )?;
        if origin.delta_v_ecef.is_none() {
            return Ok(track);
        }
        r = r1;
        v = v1;
        t0 += turn.duration_s;
    }
    if let Some(dv) = origin.delta_v_ecef {
        v = v.add(Vec3::from_ecef(dv[0], dv[1], dv[2]));
    }
    track_from_state(r, v, origin.ballistic_coeff, wind, origin.ground_alt_m, t0)
}

fn spawn_debris(
    r: Vec3,
    v: Vec3,
    time_offset: f64,
    catalog: &DebrisCatalog,
    seed: u64,
    wind: &WindSpec,
    name_prefix: &str,
    source: &StateSample,
    turn: Option<TurnOrigin>,
    ground_alt_m: f64,
) -> Result<Vec<BuiltTrack>, String> {
    let mut rng = Rng::new(seed.max(1));
    let mut jobs = Vec::new();
    for piece in &catalog.pieces {
        for i in 1..=piece.count {
            let dir = rng.unit_vec();
            let dv = dir.scale(piece.delta_v_mps);
            let slug = slug(&piece.name);
            let name = if name_prefix.is_empty() {
                format!("{slug}-{i:02}")
            } else {
                format!("{name_prefix}-{slug}-{i:02}")
            };
            jobs.push((name, piece.ballistic_coeff, dv));
        }
    }
    jobs.into_par_iter()
        .map(|(name, beta, dv)| {
            let v_piece = v.add(dv);
            let parsed = track_from_state(r, v_piece, beta, wind, ground_alt_m, time_offset)?;
            Ok(BuiltTrack {
                name,
                parsed,
                origin: SimulateOrigin {
                    r_ecef: r.to_array(),
                    v_ecef: v_piece.to_array(),
                    ballistic_coeff: beta,
                    time_offset,
                    turn: turn.clone(),
                    source_track_id: Some(source.track_id),
                    source_time_s: Some(source.time_s),
                    delta_v_ecef: Some(dv.to_array()),
                    ground_alt_m,
                },
                weight: 1.0,
            })
        })
        .collect()
}

fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.trim_matches('-');
    if s.is_empty() {
        "piece".into()
    } else {
        s.to_string()
    }
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    fn gauss(&mut self) -> f64 {
        let u = (self.unit()).clamp(1e-12, 1.0 - 1e-12);
        let v = (self.unit()).clamp(1e-12, 1.0 - 1e-12);
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }

    fn unit_vec(&mut self) -> Vec3 {
        for _ in 0..16 {
            let v = Vec3::new(self.gauss(), self.gauss(), self.gauss());
            if v.norm() > 1e-8 {
                return v.normalized();
            }
        }
        Vec3::new(1.0, 0.0, 0.0)
    }
}

/// Sample times along a clock span, inclusive of the ends when possible.
#[allow(dead_code)]
pub fn sample_times(start: f64, end: f64, every_s: f64) -> Vec<f64> {
    if !start.is_finite() || !end.is_finite() || end < start {
        return Vec::new();
    }
    let step = every_s.abs().max(0.1);
    let mut times = Vec::new();
    let mut t = start;
    while t <= end + 1e-9 {
        times.push(t);
        t += step;
        if times.len() >= 400 {
            break;
        }
    }
    if times.last().map(|last| (end - last).abs() > step * 0.25).unwrap_or(true) {
        times.push(end);
    }
    times
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lla_line() -> (Vec<f32>, Vec<f64>) {
        let mut lla = Vec::new();
        let mut times = Vec::new();
        for i in 0..11 {
            let t = i as f64;
            times.push(t);
            lla.push((-106.4 + t * 0.01) as f32);
            lla.push(32.4);
            lla.push((80_000.0 - t * 1_000.0) as f32);
        }
        (lla, times)
    }

    fn lla_vertical() -> (Vec<f32>, Vec<f64>) {
        let mut lla = Vec::new();
        let mut times = Vec::new();
        for i in 0..11 {
            let t = i as f64;
            times.push(t);
            lla.push(-106.4);
            lla.push(32.4);
            lla.push((2_000.0 + t * 1_500.0) as f32);
        }
        (lla, times)
    }

    #[test]
    fn samples_mid_track_velocity() {
        let (lla, times) = lla_line();
        let s = sample_track_state(1, &lla, Some(&times), 5.0).unwrap();
        assert!((s.time_s - 5.0).abs() < 1e-9);
        assert!(s.speed_mps > 100.0, "speed {}", s.speed_mps);
        assert!((s.alt_m - 75_000.0).abs() < 2.0);
    }

    fn white_sands_spec() -> crate::generate::GenerateSpec {
        crate::generate::GenerateSpec {
            launch_lat: 32.4,
            launch_lon: -106.4,
            launch_alt_m: 0.0,
            aim_lat: 34.0,
            aim_lon: -99.0,
            aim_alt_m: 0.0,
            ballistic_coeff: 2500.0,
            burnout_alt_m: 80_000.0,
            failure_count: 0,
            wind: WindSpec::Off,
        }
    }

    fn impact_lla(lla: &[f32]) -> (f64, f64, f64) {
        let n = lla.len() / 3;
        (
            lla[(n - 1) * 3] as f64,
            lla[(n - 1) * 3 + 1] as f64,
            lla[(n - 1) * 3 + 2] as f64,
        )
    }

    fn ground_range_m(lon0: f64, lat0: f64, lon1: f64, lat1: f64) -> f64 {
        let p0 = Vec3::from_lla(lat0, lon0, 0.0);
        let p1 = Vec3::from_lla(lat1, lon1, 0.0);
        p0.sub(p1).norm()
    }

    #[test]
    fn sampled_coast_matches_parent_impact() {
        let spec = white_sands_spec();
        let track = crate::generate::generate_track(&spec).unwrap();
        let times = track.times.as_ref().unwrap();
        let (plon, plat, _) = impact_lla(&track.lla);
        let t0 = times[0];
        let t1 = *times.last().unwrap();
        for frac in [0.12, 0.35, 0.6, 0.8] {
            let t = t0 + frac * (t1 - t0);
            let sample = sample_track_state(1, &track.lla, track.times.as_deref(), t).unwrap();
            let coast = track_from_state(
                sample.position(),
                sample.velocity(),
                spec.ballistic_coeff,
                &WindSpec::Off,
                0.0,
                sample.time_s,
            )
            .unwrap();
            let (lon, lat, alt) = impact_lla(&coast.lla);
            let miss = ground_range_m(plon, plat, lon, lat);
            assert!(
                miss < 8_000.0,
                "frac={frac} miss={miss:.0}m parent=({plon:.3},{plat:.3}) coast=({lon:.3},{lat:.3}) alt={alt:.0} spd={:.0} fpa={:.1}",
                sample.speed_mps,
                sample.flight_path_deg
            );
        }
    }

    #[test]
    fn light_debris_from_early_breakup_falls_short_of_parent() {
        let spec = white_sands_spec();
        let track = crate::generate::generate_track(&spec).unwrap();
        let times = track.times.as_ref().unwrap();
        let (plon, plat, _) = impact_lla(&track.lla);
        let t = times[0] + 0.12 * (times.last().unwrap() - times[0]);
        let sample = sample_track_state(1, &track.lla, track.times.as_deref(), t).unwrap();
        let coast = track_from_state(
            sample.position(),
            sample.velocity(),
            20.0,
            &WindSpec::Off,
            0.0,
            sample.time_s,
        )
        .unwrap();
        let (lon, lat, _) = impact_lla(&coast.lla);
        let miss = ground_range_m(plon, plat, lon, lat);
        assert!(
            miss > 15_000.0,
            "low-β early debris should fall short of the parent IP, miss={miss:.0}m parent=({plon:.3},{plat:.3}) debris=({lon:.3},{lat:.3}) spd={:.0}",
            sample.speed_mps
        );
    }

    #[test]
    fn spent_stage_stops_at_parent_pad_not_ellipsoid() {
        let spec = crate::generate::GenerateSpec {
            launch_alt_m: 1_400.0,
            aim_alt_m: 1_400.0,
            ..white_sands_spec()
        };
        let track = crate::generate::generate_track(&spec).unwrap();
        let floor = ground_floor_alt(&track.lla);
        assert!(
            (floor - 1_400.0).abs() < 80.0,
            "parent landing floor should be pad HAE, got {floor}"
        );
        let times = track.times.as_ref().unwrap();
        let t = times[0] + 0.35 * (times.last().unwrap() - times[0]);
        let sample = sample_track_state(1, &track.lla, track.times.as_deref(), t).unwrap();
        let spec_stage = SpentStageSpec {
            source_track_id: 1,
            time_s: t,
            ballistic_coeff: spec.ballistic_coeff,
            object_name: Some("Stage".into()),
            mode_name: "Staging".into(),
            ..Default::default()
        };
        let past = build_spent_stage(&sample, &spec_stage, &WindSpec::Off, 0.0).unwrap();
        let matched = build_spent_stage(&sample, &spec_stage, &WindSpec::Off, floor).unwrap();
        let (plon, plat, palt) = impact_lla(&track.lla);
        let (zlon, _zlat, zalt) = impact_lla(&past.parsed.lla);
        let (mlon, mlat, malt) = impact_lla(&matched.parsed.lla);
        assert!(
            zalt < 80.0,
            "ellipsoid floor continues to HAE 0, alt={zalt}"
        );
        assert!(
            (malt - floor).abs() < 80.0,
            "stage should stop at parent pad {floor}, alt={malt}"
        );
        let overshoot = ground_range_m(plon, plat, zlon, plat);
        let stay = ground_range_m(plon, plat, mlon, mlat);
        assert!(
            stay < overshoot + 1_000.0,
            "pad floor should not fly past the parent landing stay={stay:.0} ellipsoid={overshoot:.0} parent_alt={palt:.0}"
        );
        assert!(
            stay < 12_000.0,
            "same-β stage should land near the parent, miss={stay:.0}m"
        );
    }

    #[test]
    fn default_catalog_validates() {
        DebrisCatalog::default_fts().validate().unwrap();
    }

    #[test]
    fn rng_unit_vectors_are_unit() {
        let mut rng = Rng::new(7);
        for _ in 0..32 {
            let v = rng.unit_vec();
            assert!((v.norm() - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn spent_stage_reaches_ground() {
        let (lla, times) = lla_line();
        let sample = sample_track_state(1, &lla, Some(&times), 2.0).unwrap();
        let spec = SpentStageSpec {
            source_track_id: 1,
            time_s: 2.0,
            ballistic_coeff: 200.0,
            object_name: Some("Stage".into()),
            mode_name: "Staging".into(),
            ..Default::default()
        };
        let built = build_spent_stage(&sample, &spec, &WindSpec::Off, 0.0).unwrap();
        let n = built.parsed.lla.len() / 3;
        let alt = built.parsed.lla[n * 3 - 1];
        assert!(alt.abs() < 80.0, "impact alt {alt}");
    }

    #[test]
    fn spent_stage_moves_when_wind_changes() {
        let (lla, times) = lla_line();
        let sample = sample_track_state(1, &lla, Some(&times), 2.0).unwrap();
        let spec = SpentStageSpec {
            source_track_id: 1,
            time_s: 2.0,
            ballistic_coeff: 80.0,
            object_name: Some("Stage".into()),
            mode_name: "Staging".into(),
            ..Default::default()
        };
        let calm = build_spent_stage(&sample, &spec, &WindSpec::Off, 0.0).unwrap();
        let windy = regenerate_simulated(
            &calm.origin,
            &WindSpec::Constant {
                speed_mps: 40.0,
                from_deg: 270.0,
            },
            None,
        )
        .unwrap();
        let n0 = calm.parsed.lla.len() / 3;
        let n1 = windy.lla.len() / 3;
        let lon0 = calm.parsed.lla[(n0 - 1) * 3];
        let lon1 = windy.lla[(n1 - 1) * 3];
        assert!(
            (lon0 - lon1).abs() > 0.002,
            "wind should move spent-stage impact {lon0} vs {lon1}"
        );
    }

    #[test]
    fn fts_spawns_catalog_count() {
        let (lla, times) = lla_line();
        let sample = sample_track_state(1, &lla, Some(&times), 1.0).unwrap();
        let cat = DebrisCatalog {
            id: 1,
            name: "tiny".into(),
            pieces: vec![piece("Tank", 400.0, 50.0, 2), piece("Skin", 40.0, 80.0, 3)],
        };
        let tracks = build_fts_debris(&sample, &cat, 1, &WindSpec::Off, "", 0.0).unwrap();
        assert_eq!(tracks.len(), 5);
        for t in &tracks {
            let n = t.parsed.lla.len() / 3;
            assert!(t.parsed.lla[n * 3 - 1].abs() < 120.0);
        }
    }

    #[test]
    fn nav_turn_offsets_then_breaks_up() {
        let (lla, times) = lla_line();
        let sample = sample_track_state(1, &lla, Some(&times), 1.0).unwrap();
        let cat = DebrisCatalog {
            id: 1,
            name: "tiny".into(),
            pieces: vec![piece("Tank", 400.0, 30.0, 1)],
        };
        let spec = NavFailSpec {
            source_track_id: 1,
            times_s: vec![1.0],
            catalog_id: 1,
            object_id: None,
            object_name: None,
            mode_name: "Nav + FTS".into(),
            max_g: 5.0,
            turn_duration_s: 5.0,
            turn_side: TurnSide::Both,
            sustain_speed: true,
            seed: 1,
        };
        let tracks = build_nav_failure(&sample, &spec, &cat, &WindSpec::Off, 0.0).unwrap();
        // 2 turns + 2 debris
        assert_eq!(tracks.len(), 4);
        let debris: Vec<_> = tracks.iter().filter(|t| t.weight > 0.0).collect();
        assert_eq!(debris.len(), 2);
        for t in &debris {
            let n = t.parsed.lla.len() / 3;
            let alt = t.parsed.lla[n * 3 - 1];
            assert!(alt.abs() < 120.0, "debris should reach the ground, alt={alt}");
        }
        let turns: Vec<_> = tracks.iter().filter(|t| t.weight == 0.0).collect();
        assert_eq!(turns.len(), 2);
        let (lon_a, lat_a) = {
            let lla = &turns[0].parsed.lla;
            let n = lla.len() / 3;
            (lla[(n - 1) * 3], lla[(n - 1) * 3 + 1])
        };
        let (lon_b, lat_b) = {
            let lla = &turns[1].parsed.lla;
            let n = lla.len() / 3;
            (lla[(n - 1) * 3], lla[(n - 1) * 3 + 1])
        };
        let sep = ((lon_a - lon_b).powi(2) + (lat_a - lat_b).powi(2)).sqrt();
        assert!(sep > 0.001, "left/right turns should split {lon_a},{lat_a} vs {lon_b},{lat_b}");
    }

    #[test]
    fn nav_turn_works_when_velocity_is_vertical() {
        let (lla, times) = lla_vertical();
        let sample = sample_track_state(1, &lla, Some(&times), 5.0).unwrap();
        assert!(sample.alt_m > 8_000.0);
        let cat = DebrisCatalog {
            id: 1,
            name: "tiny".into(),
            pieces: vec![piece("Tank", 400.0, 30.0, 1)],
        };
        let spec = NavFailSpec {
            source_track_id: 1,
            times_s: vec![5.0],
            catalog_id: 1,
            object_id: None,
            object_name: None,
            mode_name: "Nav + FTS".into(),
            max_g: 5.0,
            turn_duration_s: 5.0,
            turn_side: TurnSide::Both,
            sustain_speed: true,
            seed: 1,
        };
        let tracks = build_nav_failure(&sample, &spec, &cat, &WindSpec::Off, 0.0).unwrap();
        let debris: Vec<_> = tracks.iter().filter(|t| t.weight > 0.0).collect();
        assert_eq!(debris.len(), 2);
        let turns: Vec<_> = tracks.iter().filter(|t| t.weight == 0.0).collect();
        assert_eq!(turns.len(), 2);
        let end = |track: &BuiltTrack| {
            let lla = &track.parsed.lla;
            let n = lla.len() / 3;
            (lla[(n - 1) * 3], lla[(n - 1) * 3 + 1], lla[(n - 1) * 3 + 2], n)
        };
        let (lon_a, lat_a, alt_a, n_a) = end(turns[0]);
        let (lon_b, lat_b, alt_b, n_b) = end(turns[1]);
        let sep = ((lon_a - lon_b).powi(2) + (lat_a - lat_b).powi(2)).sqrt();
        assert!(
            sep > 0.0005,
            "vertical flight should still yaw east/west {lon_a},{lat_a} vs {lon_b},{lat_b} alt={alt_a}/{alt_b} n={n_a}/{n_b} heading={} fpa={} spd={} sample_alt={}",
            sample.heading_deg,
            sample.flight_path_deg,
            sample.speed_mps,
            sample.alt_m
        );
    }

    #[test]
    fn sample_times_includes_ends() {
        let t = sample_times(10.0, 40.0, 10.0);
        assert_eq!(t.first().copied(), Some(10.0));
        assert_eq!(t.last().copied(), Some(40.0));
        assert!(t.len() >= 4);
    }

    #[test]
    fn uniform_separation_stays_in_window() {
        let spec = SpentStageSpec {
            dist: TimeDist::Uniform,
            time_s: 20.0,
            count: 40,
            t_min: Some(12.0),
            t_max: Some(18.0),
            seed: 3,
            ..Default::default()
        };
        let times = sample_stage_times(&spec, 0.0, 60.0).unwrap();
        assert_eq!(times.len(), 40);
        assert!(times.iter().all(|t| (12.0..=18.0).contains(t)));
        let min = times.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = times.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(max - min > 4.0, "samples should spread across the window {min}..{max}");
    }

    #[test]
    fn normal_separation_clusters_near_mean() {
        let spec = SpentStageSpec {
            dist: TimeDist::Normal,
            time_s: 30.0,
            count: 80,
            sigma_s: Some(1.5),
            n_sigma: 3.0,
            seed: 9,
            ..Default::default()
        };
        let times = sample_stage_times(&spec, 0.0, 60.0).unwrap();
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        assert!((mean - 30.0).abs() < 0.6, "mean {mean}");
        assert!(times.iter().all(|t| (25.5..=34.5).contains(t)));
    }

    #[test]
    fn normal_window_is_plus_minus_n_sigma() {
        let spec = SpentStageSpec {
            dist: TimeDist::Normal,
            time_s: 40.0,
            count: 50,
            sigma_s: Some(2.0),
            n_sigma: 2.0,
            seed: 4,
            ..Default::default()
        };
        let times = sample_stage_times(&spec, 0.0, 80.0).unwrap();
        assert!(times.iter().all(|t| (36.0..=44.0).contains(t)));
    }
}
