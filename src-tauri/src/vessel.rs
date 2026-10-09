//! Vessel strike risk on one fixed-resolution Uber H3 grid.
//!
//! Objects are mutually inclusive. Trajectories of one object are mutually
//! exclusive; their weights are normalized to 1 when they do not already sum
//! to 1. Debris pieces from one trajectory in one Monte Carlo trial are
//! correlated and combined with the product union inside each cell.
//!
//! `R_t` is then smoothed by Botev's diffusion KDE. Bandwidth is the
//! bivariate FFT / heat-equation fixed point from
//! Z. I. Botev, J. F. Grotowski, and D. P. Kroese, "Kernel density estimation
//! via diffusion," Annals of Statistics 38(5), 2916–2957, 2010, following the
//! reference implementation `kde2d.m` (Z. Botev). The same diffusion time is
//! applied to the trajectory risk matrix. A fixed Gaussian bandwidth is not
//! used. When the impact sample has no measurable spread, the risk matrix is
//! left unchanged because a bandwidth is not identifiable.

use std::collections::{BTreeMap, HashMap, HashSet};

use h3o::{CellIndex, LatLng, Resolution};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::{Deserialize, Serialize};

/// Resolution 8 hexes are about 0.74 km², small enough to separate nearby
/// boats and large enough to draw a regional debris field.
pub const H3_RES: Resolution = Resolution::Eight;
pub const H3_RESOLUTION: u8 = 8;
pub const MAX_TRIALS: u32 = 64;
/// Floor on |sin(gamma)| so a grazing path cannot make A_v unbounded.
pub const SIN_GAMMA_MIN: f64 = 1.0e-2;
const WEIGHT_TOL: f64 = 1.0e-6;
const BOTEV_N: usize = 128;
const DRAW_LIMIT: usize = 2_500;
const DRAW_MIN_RISK: f64 = 1.0e-6;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeightResolution {
    Unchanged,
    Normalized { sum: f64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VesselParams {
    #[serde(default = "default_ke")]
    pub ke_threshold_j: f64,
    #[serde(default = "default_area")]
    pub a_proj_m2: f64,
    #[serde(default = "default_perim")]
    pub p_proj_m: f64,
    #[serde(default = "default_trials")]
    pub max_trials: u32,
    #[serde(default = "default_mass")]
    pub default_mass_kg: f64,
    #[serde(default = "default_radius")]
    pub default_radius_m: f64,
}

fn default_ke() -> f64 {
    0.0
}
fn default_area() -> f64 {
    80.0
}
fn default_perim() -> f64 {
    40.0
}
fn default_trials() -> u32 {
    32
}
fn default_mass() -> f64 {
    50.0
}
fn default_radius() -> f64 {
    0.3
}

impl Default for VesselParams {
    fn default() -> Self {
        Self {
            ke_threshold_j: default_ke(),
            a_proj_m2: default_area(),
            p_proj_m: default_perim(),
            max_trials: default_trials(),
            default_mass_kg: default_mass(),
            default_radius_m: default_radius(),
        }
    }
}

impl VesselParams {
    pub fn clamped(mut self) -> Result<Self, String> {
        if !self.ke_threshold_j.is_finite() || self.ke_threshold_j < 0.0 {
            return Err("KE threshold must be a non-negative number of joules".into());
        }
        if !self.a_proj_m2.is_finite() || self.a_proj_m2 < 0.0 {
            return Err("vessel presented area A_proj must be ≥ 0 m²".into());
        }
        if !self.p_proj_m.is_finite() || self.p_proj_m < 0.0 {
            return Err("vessel presented perimeter P_proj must be ≥ 0 m".into());
        }
        if !self.default_mass_kg.is_finite() || self.default_mass_kg <= 0.0 {
            return Err("default debris mass must be positive".into());
        }
        if !self.default_radius_m.is_finite() || self.default_radius_m < 0.0 {
            return Err("default debris radius must be ≥ 0 m".into());
        }
        if self.max_trials == 0 {
            return Err("Monte Carlo trial cap N must be at least 1".into());
        }
        self.max_trials = self.max_trials.min(MAX_TRIALS);
        Ok(self)
    }
}

#[derive(Debug, Clone)]
pub struct PieceInput {
    pub lon: f64,
    pub lat: f64,
    pub mass_kg: f64,
    pub radius_m: f64,
    pub v_east: f64,
    pub v_north: f64,
    pub v_up: f64,
}

#[derive(Debug, Clone)]
pub struct TrajectoryInput {
    pub weight: f64,
    pub trials: Vec<Vec<PieceInput>>,
}

#[derive(Debug, Clone)]
pub struct ObjectInput {
    pub name: String,
    pub trajectories: Vec<TrajectoryInput>,
}

#[derive(Debug, Clone)]
pub struct BoatQuery {
    pub id: u64,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    pub people: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoatScore {
    pub id: u64,
    pub name: String,
    pub people: Option<u32>,
    /// P_b = Risk(g) of the H3 cell that contains the boat.
    pub strike: f64,
    /// P_b times people on board. Absent when the headcount is unknown.
    pub individual: Option<f64>,
    pub h3: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskCell {
    pub h3: String,
    pub risk: f64,
    /// Closed-enough ring as lon, lat, lon, lat, … in degrees.
    pub boundary: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VesselRisk {
    pub cells: Vec<RiskCell>,
    pub collective: f64,
    pub boat_count: usize,
    pub boats: Vec<BoatScore>,
    pub area_m2: f64,
    pub resolution: u8,
    pub notes: Vec<String>,
    pub bandwidth_east_m: Option<f64>,
    pub bandwidth_north_m: Option<f64>,
    pub smoothed: bool,
}

/// Average hexagon area at [`H3_RES`]. Every risk cell uses this one A_g.
pub fn cell_area_m2() -> f64 {
    H3_RES.area_m2()
}

pub fn cell_index(lon: f64, lat: f64) -> Option<CellIndex> {
    let ll = LatLng::new(lat, lon).ok()?;
    Some(ll.to_cell(H3_RES))
}

/// Trajectory weights on one object. Negative weights and a non-positive sum
/// are rejected. Any other positive set that does not already sum to 1 is
/// divided by its sum. That normalization is the explicit behavior.
pub fn resolve_trajectory_weights(weights: &[f64]) -> Result<(Vec<f64>, WeightResolution), String> {
    if weights.is_empty() {
        return Ok((Vec::new(), WeightResolution::Unchanged));
    }
    let mut sum = 0.0;
    for (i, w) in weights.iter().copied().enumerate() {
        if !w.is_finite() {
            return Err(format!("trajectory weight {i} is not finite"));
        }
        if w < 0.0 {
            return Err(format!("trajectory weight {w} is negative"));
        }
        sum += w;
    }
    if sum <= 0.0 {
        return Err("trajectory weights sum to 0".into());
    }
    if (sum - 1.0).abs() <= WEIGHT_TOL {
        return Ok((weights.to_vec(), WeightResolution::Unchanged));
    }
    let probs: Vec<f64> = weights.iter().map(|w| w / sum).collect();
    Ok((probs, WeightResolution::Normalized { sum }))
}

pub fn kinetic_energy_j(mass_kg: f64, speed_mps: f64) -> f64 {
    0.5 * mass_kg * speed_mps * speed_mps
}

pub fn speed_mps(v_east: f64, v_north: f64, v_up: f64) -> f64 {
    (v_east * v_east + v_north * v_north + v_up * v_up).sqrt()
}

/// Elevation of the velocity above the local horizontal, radians.
/// Descending debris has a negative gamma; the area formula uses |sin|.
pub fn gamma_rad(v_east: f64, v_north: f64, v_up: f64) -> f64 {
    let horizontal = (v_east * v_east + v_north * v_north).sqrt();
    v_up.atan2(horizontal)
}

pub fn vulnerability_area_m2(a_proj: f64, p_proj: f64, radius_m: f64, gamma: f64) -> f64 {
    let a_perp = a_proj + p_proj * radius_m + std::f64::consts::PI * radius_m * radius_m;
    let s = gamma.sin().abs().max(SIN_GAMMA_MIN);
    a_perp / s
}

/// `1 - Π (1 - min(1, A_v / A_g))`. An empty threat set is 0.
pub fn trial_cell_risk(areas: impl IntoIterator<Item = f64>, a_g: f64) -> f64 {
    if a_g <= 0.0 {
        return 0.0;
    }
    let mut survive = 1.0;
    let mut any = false;
    for a_v in areas {
        if !a_v.is_finite() || a_v <= 0.0 {
            continue;
        }
        any = true;
        let ratio = (a_v / a_g).clamp(0.0, 1.0);
        survive *= 1.0 - ratio;
    }
    if any {
        1.0 - survive
    } else {
        0.0
    }
}

pub fn mean_trials(trials: &[HashMap<u64, f64>]) -> HashMap<u64, f64> {
    let n = trials.len();
    if n == 0 {
        return HashMap::new();
    }
    let mut acc: HashMap<u64, f64> = HashMap::new();
    for trial in trials {
        for (cell, r) in trial {
            *acc.entry(*cell).or_insert(0.0) += *r;
        }
    }
    let scale = 1.0 / n as f64;
    for r in acc.values_mut() {
        *r *= scale;
    }
    acc
}

pub fn combine_trajectories(weighted: &[(f64, HashMap<u64, f64>)]) -> HashMap<u64, f64> {
    let mut acc: HashMap<u64, f64> = HashMap::new();
    for (p, field) in weighted {
        if *p <= 0.0 {
            continue;
        }
        for (cell, r) in field {
            *acc.entry(*cell).or_insert(0.0) += *p * r.clamp(0.0, 1.0);
        }
    }
    acc
}

pub fn combine_objects(objects: &[HashMap<u64, f64>]) -> HashMap<u64, f64> {
    let mut cells: HashSet<u64> = HashSet::new();
    for field in objects {
        cells.extend(field.keys().copied());
    }
    let mut out = HashMap::new();
    for cell in cells {
        let mut miss = 1.0;
        for field in objects {
            let r = field.get(&cell).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            miss *= 1.0 - r;
        }
        let risk = 1.0 - miss;
        if risk > 0.0 {
            out.insert(cell, risk);
        }
    }
    out
}

pub fn individual_risk(strike: f64, people: u32) -> f64 {
    strike.clamp(0.0, 1.0) * f64::from(people)
}

pub fn collective_risk(strikes: &[f64]) -> f64 {
    if strikes.is_empty() {
        return 0.0;
    }
    strikes.iter().map(|p| p.clamp(0.0, 1.0)).sum::<f64>() / strikes.len() as f64
}

pub fn score_boats(risk: &HashMap<u64, f64>, boats: &[BoatQuery]) -> (Vec<BoatScore>, f64) {
    let mut scores = Vec::with_capacity(boats.len());
    let mut strikes = Vec::with_capacity(boats.len());
    for boat in boats {
        let (strike, h3) = match cell_index(boat.lon, boat.lat) {
            Some(cell) => {
                let key = u64::from(cell);
                (risk.get(&key).copied().unwrap_or(0.0).clamp(0.0, 1.0), cell.to_string())
            }
            None => (0.0, String::new()),
        };
        strikes.push(strike);
        let individual = boat.people.map(|n| individual_risk(strike, n));
        scores.push(BoatScore {
            id: boat.id,
            name: boat.name.clone(),
            people: boat.people,
            strike,
            individual,
            h3,
        });
    }
    (scores, collective_risk(&strikes))
}

pub fn compute_vessel_risk(objects: &[ObjectInput], params: &VesselParams, boats: &[BoatQuery]) -> Result<VesselRisk, String> {
    let params = params.clone().clamped()?;
    let a_g = cell_area_m2();
    let cap = params.max_trials.clamp(1, MAX_TRIALS) as usize;
    let mut notes = Vec::new();
    let mut object_fields = Vec::new();
    let mut bw_e = None;
    let mut bw_n = None;
    let mut smoothed = false;

    for object in objects {
        let weights: Vec<f64> = object.trajectories.iter().map(|t| t.weight).collect();
        let (probs, how) = resolve_trajectory_weights(&weights)?;
        if let WeightResolution::Normalized { sum } = how {
            notes.push(format!(
                "{}: trajectory weights summed to {sum:.6} and were normalized so they sum to 1",
                object.name
            ));
        }
        let mut weighted = Vec::new();
        for (traj, p) in object.trajectories.iter().zip(probs) {
            let (field, bw) = trajectory_risk(traj, &params, a_g, cap);
            if let Some((east, north)) = bw {
                smoothed = true;
                bw_e = Some(bw_e.map(|v: f64| v.max(east)).unwrap_or(east));
                bw_n = Some(bw_n.map(|v: f64| v.max(north)).unwrap_or(north));
            }
            weighted.push((p, field));
        }
        object_fields.push(combine_trajectories(&weighted));
    }

    let risk = combine_objects(&object_fields);
    let (boat_scores, collective) = score_boats(&risk, boats);
    let cells = draw_cells(&risk);

    Ok(VesselRisk {
        cells,
        collective,
        boat_count: boats.len(),
        boats: boat_scores,
        area_m2: a_g,
        resolution: H3_RESOLUTION,
        notes,
        bandwidth_east_m: bw_e,
        bandwidth_north_m: bw_n,
        smoothed,
    })
}

fn trajectory_risk(
    traj: &TrajectoryInput,
    params: &VesselParams,
    a_g: f64,
    cap: usize,
) -> (HashMap<u64, f64>, Option<(f64, f64)>) {
    let n = traj.trials.len().min(cap);
    if n == 0 {
        return (HashMap::new(), None);
    }
    let mut trials = Vec::with_capacity(n);
    let mut points: Vec<[f64; 2]> = Vec::new();
    for trial in traj.trials.iter().take(n) {
        let mut by_cell: HashMap<u64, Vec<f64>> = HashMap::new();
        for piece in trial {
            let speed = speed_mps(piece.v_east, piece.v_north, piece.v_up);
            let ke = kinetic_energy_j(piece.mass_kg, speed);
            if ke < params.ke_threshold_j {
                continue;
            }
            let Some(cell) = cell_index(piece.lon, piece.lat) else {
                continue;
            };
            let gamma = gamma_rad(piece.v_east, piece.v_north, piece.v_up);
            let area = vulnerability_area_m2(params.a_proj_m2, params.p_proj_m, piece.radius_m, gamma);
            by_cell.entry(u64::from(cell)).or_default().push(area);
            points.push([piece.lon, piece.lat]);
        }
        let field = by_cell
            .into_iter()
            .map(|(cell, areas)| (cell, trial_cell_risk(areas, a_g)))
            .filter(|(_, r)| *r > 0.0)
            .collect();
        trials.push(field);
    }
    let raw = mean_trials(&trials);
    match smooth_risk(&raw, &points) {
        Some((field, east, north)) => (field, Some((east, north))),
        None => (raw, None),
    }
}

fn smooth_risk(field: &HashMap<u64, f64>, lonlat: &[[f64; 2]]) -> Option<(HashMap<u64, f64>, f64, f64)> {
    if field.is_empty() || lonlat.len() < 2 {
        return None;
    }
    let frame = LocalFrame::from_lonlat(lonlat)?;
    let mut metres = Vec::with_capacity(lonlat.len());
    for p in lonlat {
        metres.push(frame.to_metres(p[0], p[1]));
    }
    let botev = botev_bandwidth(&metres)?;
    let raster = rasterize_field(field, &frame, &botev);
    let diffused = diffuse_field(&raster, BOTEV_N, botev.t_x, botev.t_y);
    let edge = H3_RES.edge_length_m().max(1.0);
    let reach = 4.0 * botev.bandwidth_x.max(botev.bandwidth_y);
    let k = ((reach / edge).ceil() as u32).clamp(1, 24);
    let mut seen: HashSet<u64> = HashSet::new();
    let mut out = HashMap::new();
    for &cell_raw in field.keys() {
        let Ok(cell) = CellIndex::try_from(cell_raw) else {
            continue;
        };
        let disk: Vec<CellIndex> = cell.grid_disk(k);
        for neighbor in disk {
            let key = u64::from(neighbor);
            if !seen.insert(key) {
                continue;
            }
            let center = LatLng::from(neighbor);
            let [east, north] = frame.to_metres(center.lng(), center.lat());
            let value = sample_grid(&diffused, BOTEV_N, &botev, east, north).clamp(0.0, 1.0);
            if value > 0.0 {
                out.insert(key, value);
            }
        }
    }
    Some((out, botev.bandwidth_x, botev.bandwidth_y))
}

fn rasterize_field(field: &HashMap<u64, f64>, frame: &LocalFrame, botev: &BotevBand) -> Vec<f64> {
    let n = BOTEV_N;
    let mut grid = vec![0.0; n * n];
    for x in 0..n {
        let east = botev.min_x + (x as f64 + 0.5) / n as f64 * botev.scale_x;
        for y in 0..n {
            let north = botev.min_y + (y as f64 + 0.5) / n as f64 * botev.scale_y;
            let (lon, lat) = frame.to_lonlat(east, north);
            if let Some(cell) = cell_index(lon, lat) {
                if let Some(r) = field.get(&u64::from(cell)) {
                    grid[x * n + y] = r.clamp(0.0, 1.0);
                }
            }
        }
    }
    grid
}

fn sample_grid(grid: &[f64], n: usize, botev: &BotevBand, east: f64, north: f64) -> f64 {
    if botev.scale_x <= 0.0 || botev.scale_y <= 0.0 {
        return 0.0;
    }
    let fx = ((east - botev.min_x) / botev.scale_x * n as f64 - 0.5).clamp(0.0, (n - 1) as f64);
    let fy = ((north - botev.min_y) / botev.scale_y * n as f64 - 0.5).clamp(0.0, (n - 1) as f64);
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(n - 1);
    let y1 = (y0 + 1).min(n - 1);
    let tx = fx - x0 as f64;
    let ty = fy - y0 as f64;
    let v = |x, y| grid[x * n + y];
    (1.0 - ty) * ((1.0 - tx) * v(x0, y0) + tx * v(x1, y0)) + ty * ((1.0 - tx) * v(x0, y1) + tx * v(x1, y1))
}

fn draw_cells(risk: &HashMap<u64, f64>) -> Vec<RiskCell> {
    let mut ranked: Vec<(u64, f64)> = risk
        .iter()
        .map(|(c, r)| (*c, *r))
        .filter(|(_, r)| *r >= DRAW_MIN_RISK)
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(DRAW_LIMIT);
    let mut cells = Vec::with_capacity(ranked.len());
    for (raw, risk) in ranked {
        let Ok(cell) = CellIndex::try_from(raw) else {
            continue;
        };
        let mut boundary = Vec::new();
        for ll in cell.boundary().iter() {
            boundary.push(ll.lng());
            boundary.push(ll.lat());
        }
        cells.push(RiskCell {
            h3: cell.to_string(),
            risk,
            boundary,
        });
    }
    cells
}

struct LocalFrame {
    lon0: f64,
    lat0: f64,
    mx: f64,
    my: f64,
}

impl LocalFrame {
    fn from_lonlat(points: &[[f64; 2]]) -> Option<Self> {
        if points.is_empty() {
            return None;
        }
        let lat0 = points.iter().map(|p| p[1]).sum::<f64>() / points.len() as f64;
        let lon0 = points.iter().map(|p| p[0]).sum::<f64>() / points.len() as f64;
        let mx = 111_132.0 * lat0.to_radians().cos().abs().max(0.2);
        Some(Self {
            lon0,
            lat0,
            mx,
            my: 111_132.0,
        })
    }

    fn to_metres(&self, lon: f64, lat: f64) -> [f64; 2] {
        [(lon - self.lon0) * self.mx, (lat - self.lat0) * self.my]
    }

    fn to_lonlat(&self, east: f64, north: f64) -> (f64, f64) {
        (self.lon0 + east / self.mx, self.lat0 + north / self.my)
    }
}

struct BotevBand {
    t_x: f64,
    t_y: f64,
    min_x: f64,
    min_y: f64,
    scale_x: f64,
    scale_y: f64,
    bandwidth_x: f64,
    bandwidth_y: f64,
}

/// Botev bivariate diffusion bandwidth on points in metres. `None` when the
/// cloud has no spread or the heat-equation fixed point does not converge.
fn botev_bandwidth(points: &[[f64; 2]]) -> Option<BotevBand> {
    if points.len() < 2 {
        return None;
    }
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for p in points {
        min_x = min_x.min(p[0]);
        max_x = max_x.max(p[0]);
        min_y = min_y.min(p[1]);
        max_y = max_y.max(p[1]);
    }
    let range_x = max_x - min_x;
    let range_y = max_y - min_y;
    if range_x < 1.0 && range_y < 1.0 {
        return None;
    }
    let range_x = range_x.max(1.0);
    let range_y = range_y.max(1.0);
    let min_x = min_x - 0.25 * range_x;
    let max_x = max_x + 0.25 * range_x;
    let min_y = min_y - 0.25 * range_y;
    let max_y = max_y + 0.25 * range_y;
    let scale_x = max_x - min_x;
    let scale_y = max_y - min_y;
    if scale_x <= 0.0 || scale_y <= 0.0 {
        return None;
    }
    let n = BOTEV_N;
    let mut unit = Vec::with_capacity(points.len());
    for p in points {
        let x = ((p[0] - min_x) / scale_x).clamp(0.0, 1.0 - 1e-12);
        let y = ((p[1] - min_y) / scale_y).clamp(0.0, 1.0 - 1e-12);
        unit.push([x, y]);
    }
    let hist = ndhist(&unit, n);
    let a = dct2d(&hist, n);
    let a2: Vec<f64> = a.iter().map(|v| v * v).collect();
    let i_sq: Vec<f64> = (0..n).map(|k| (k as f64) * (k as f64)).collect();
    let n_samples = points.len() as f64;
    let mut solver = BotevSolver {
        n_samples,
        i_sq: &i_sq,
        a2: &a2,
        n,
        memo: HashMap::new(),
    };
    let t_star = botev_root(&mut solver)?;
    let p_02 = solver.func([0, 2], t_star);
    let p_20 = solver.func([2, 0], t_star);
    let p_11 = solver.func([1, 1], t_star);
    if ![p_02, p_20, p_11].iter().all(|v| v.is_finite() && *v > 0.0) {
        return None;
    }
    let t_y = (p_02.powf(0.75)
        / (4.0 * std::f64::consts::PI * n_samples * p_20.powf(0.75) * (p_11 + (p_11 * p_20 * p_02).sqrt())))
    .powf(1.0 / 3.0);
    let t_x = (p_20.powf(0.75)
        / (4.0 * std::f64::consts::PI * n_samples * p_02.powf(0.75) * (p_11 + (p_11 * p_20 * p_02).sqrt())))
    .powf(1.0 / 3.0);
    if !t_x.is_finite() || !t_y.is_finite() || t_x <= 0.0 || t_y <= 0.0 {
        return None;
    }
    Some(BotevBand {
        bandwidth_x: t_x.sqrt() * scale_x,
        bandwidth_y: t_y.sqrt() * scale_y,
        t_x,
        t_y,
        min_x,
        min_y,
        scale_x,
        scale_y,
    })
}

fn ndhist(unit: &[[f64; 2]], n: usize) -> Vec<f64> {
    let mut hist = vec![0.0; n * n];
    let inv = 1.0 / unit.len() as f64;
    for p in unit {
        let mut ix = (p[0] * n as f64).floor() as usize;
        let mut iy = (p[1] * n as f64).floor() as usize;
        if ix >= n {
            ix = n - 1;
        }
        if iy >= n {
            iy = n - 1;
        }
        hist[ix * n + iy] += inv;
    }
    hist
}

struct BotevSolver<'a> {
    n_samples: f64,
    i_sq: &'a [f64],
    a2: &'a [f64],
    n: usize,
    memo: HashMap<(i32, i32, u64), f64>,
}

impl BotevSolver<'_> {
    fn func(&mut self, s: [i32; 2], t: f64) -> f64 {
        let key = (s[0], s[1], t.to_bits());
        if let Some(v) = self.memo.get(&key) {
            return *v;
        }
        let value = self.func_inner(s, t);
        self.memo.insert(key, value);
        value
    }

    fn func_inner(&mut self, s: [i32; 2], t: f64) -> f64 {
        let order = s[0] + s[1];
        if order <= 4 {
            let sum = self.func([s[0] + 1, s[1]], t) + self.func([s[0], s[1] + 1], t);
            if !sum.is_finite() || sum == 0.0 {
                return f64::NAN;
            }
            let const_s = (1.0 + 0.5_f64.powi(order + 1)) / 3.0;
            let numer = -2.0 * const_s * k_factor(s[0]) * k_factor(s[1]);
            let base = numer / self.n_samples / sum;
            if base <= 0.0 {
                return f64::NAN;
            }
            let time = base.powf(1.0 / (2.0 + order as f64));
            psi(s, time, self.i_sq, self.a2, self.n)
        } else {
            psi(s, t, self.i_sq, self.a2, self.n)
        }
    }

    fn evolve(&mut self, t: f64) -> f64 {
        let sum = self.func([0, 2], t) + self.func([2, 0], t) + 2.0 * self.func([1, 1], t);
        if !sum.is_finite() || sum <= 0.0 {
            return f64::NAN;
        }
        let time = (2.0 * std::f64::consts::PI * self.n_samples * sum).powf(-1.0 / 3.0);
        if !time.is_finite() || time == 0.0 {
            return f64::NAN;
        }
        (t - time) / time
    }
}

fn botev_root(solver: &mut BotevSolver<'_>) -> Option<f64> {
    let mut lo = 1e-8;
    let mut hi = 0.1;
    let mut flo = solver.evolve(lo);
    let mut fhi = solver.evolve(hi);
    for _ in 0..12 {
        if flo.is_finite() && fhi.is_finite() && flo * fhi <= 0.0 {
            break;
        }
        hi *= 2.0;
        if hi > 2.0 {
            return None;
        }
        fhi = solver.evolve(hi);
        if !flo.is_finite() {
            lo *= 10.0;
            flo = solver.evolve(lo);
        }
    }
    if !flo.is_finite() || !fhi.is_finite() || flo * fhi > 0.0 {
        return None;
    }
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        let fmid = solver.evolve(mid);
        if !fmid.is_finite() {
            return None;
        }
        if fmid.abs() < 1e-10 || (hi - lo) < 1e-14 {
            return Some(mid);
        }
        if flo * fmid <= 0.0 {
            hi = mid;
        } else {
            lo = mid;
            flo = fmid;
        }
    }
    Some(0.5 * (lo + hi))
}

fn psi(s: [i32; 2], time: f64, i_sq: &[f64], a2: &[f64], n: usize) -> f64 {
    if !time.is_finite() || time < 0.0 {
        return f64::NAN;
    }
    // Match kde2d.m: wy (order s[1]) multiplies rows and wx (order s[0]) multiplies columns.
    let mut acc = 0.0;
    for row in 0..n {
        let wy = weight(row, time, i_sq) * pow_isq(i_sq[row], s[1]);
        if wy == 0.0 {
            continue;
        }
        for col in 0..n {
            let wx = weight(col, time, i_sq) * pow_isq(i_sq[col], s[0]);
            acc += wy * a2[row * n + col] * wx;
        }
    }
    let sign = if (s[0] + s[1]) % 2 == 0 { 1.0 } else { -1.0 };
    sign * acc * std::f64::consts::PI.powi(2 * (s[0] + s[1]))
}

fn pow_isq(i_sq: f64, exp: i32) -> f64 {
    if exp == 0 {
        1.0
    } else {
        i_sq.powi(exp)
    }
}

fn weight(k: usize, time: f64, i_sq: &[f64]) -> f64 {
    let amp = if k == 0 { 1.0 } else { 0.5 };
    amp * (-i_sq[k] * std::f64::consts::PI * std::f64::consts::PI * time).exp()
}

fn k_factor(s: i32) -> f64 {
    let mut prod = 1.0;
    let mut k = 1.0;
    let limit = 2.0 * s as f64;
    while k <= limit {
        prod *= k;
        k += 2.0;
    }
    let sign = if s % 2 == 0 { 1.0 } else { -1.0 };
    sign * prod / (2.0 * std::f64::consts::PI).sqrt()
}

fn diffuse_field(field: &[f64], n: usize, t_x: f64, t_y: f64) -> Vec<f64> {
    let mut a = dct2d(field, n);
    for x in 0..n {
        let gx = (-(x as f64) * (x as f64) * std::f64::consts::PI * std::f64::consts::PI * t_x / 2.0).exp();
        for y in 0..n {
            let gy = (-(y as f64) * (y as f64) * std::f64::consts::PI * std::f64::consts::PI * t_y / 2.0).exp();
            a[x * n + y] *= gx * gy;
        }
    }
    idct2d(&a, n)
}

fn dct2d(data: &[f64], n: usize) -> Vec<f64> {
    let mut a = data.to_vec();
    let mut line = vec![0.0; n];
    for y in 0..n {
        for x in 0..n {
            line[x] = a[x * n + y];
        }
        dct1(&mut line);
        for x in 0..n {
            a[x * n + y] = line[x];
        }
    }
    for x in 0..n {
        for y in 0..n {
            line[y] = a[x * n + y];
        }
        dct1(&mut line);
        for y in 0..n {
            a[x * n + y] = line[y];
        }
    }
    a
}

fn idct2d(data: &[f64], n: usize) -> Vec<f64> {
    let mut a = data.to_vec();
    let mut line = vec![0.0; n];
    for y in 0..n {
        for x in 0..n {
            line[x] = a[x * n + y];
        }
        idct1(&mut line);
        for x in 0..n {
            a[x * n + y] = line[x];
        }
    }
    for x in 0..n {
        for y in 0..n {
            line[y] = a[x * n + y];
        }
        idct1(&mut line);
        for y in 0..n {
            a[x * n + y] = line[y];
        }
    }
    a
}

fn dct1(col: &mut [f64]) {
    let n = col.len();
    let half = n / 2;
    let mut spec = vec![Complex::new(0.0, 0.0); n];
    for i in 0..half {
        spec[i] = Complex::new(col[2 * i], 0.0);
        spec[half + i] = Complex::new(col[n - 1 - 2 * i], 0.0);
    }
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(n);
    fft.process(&mut spec);
    for k in 0..n {
        let scale = if k == 0 { 1.0 } else { 2.0 };
        let w = Complex::from_polar(scale, -(k as f64) * std::f64::consts::PI / (2.0 * n as f64));
        col[k] = (w * spec[k]).re;
    }
}

fn idct1(col: &mut [f64]) {
    let n = col.len();
    let mut spec = vec![Complex::new(0.0, 0.0); n];
    for k in 0..n {
        let w = Complex::from_polar(1.0, (k as f64) * std::f64::consts::PI / (2.0 * n as f64));
        spec[k] = w * Complex::new(col[k], 0.0);
    }
    let mut planner = FftPlanner::new();
    let ifft = planner.plan_fft_inverse(n);
    ifft.process(&mut spec);
    let scale = 1.0 / n as f64;
    let half = n / 2;
    let mut out = vec![0.0; n];
    for i in 0..half {
        out[2 * i] = spec[i].re * scale;
        out[2 * i + 1] = spec[n - 1 - i].re * scale;
    }
    col.copy_from_slice(&out);
}

/// Group store tracks into exclusive trajectories and correlated trials.
pub struct TrackPiece {
    pub object_id: u64,
    pub object_name: String,
    pub mode_id: u64,
    pub probability: f64,
    pub fragment: bool,
    pub source_track_id: u64,
    pub source_time_bits: u64,
    pub turn_side_bits: u64,
    pub trial_id: u64,
    pub track_id: u64,
    pub piece: PieceInput,
}

pub fn objects_from_tracks(tracks: Vec<TrackPiece>) -> Vec<ObjectInput> {
    let mut names: BTreeMap<u64, String> = BTreeMap::new();
    let mut intact: BTreeMap<(u64, u64), TrajectoryInput> = BTreeMap::new();
    #[derive(Clone)]
    struct Cloud {
        weight: f64,
        trials: BTreeMap<u64, Vec<PieceInput>>,
    }
    let mut clouds: BTreeMap<(u64, u64, u64, u64, u64), Cloud> = BTreeMap::new();
    for track in tracks {
        names.insert(track.object_id, track.object_name.clone());
        if track.probability <= 0.0 {
            continue;
        }
        if !track.fragment {
            intact.insert(
                (track.object_id, track.track_id),
                TrajectoryInput {
                    weight: track.probability,
                    trials: vec![vec![track.piece]],
                },
            );
            continue;
        }
        let key = (
            track.object_id,
            track.mode_id,
            track.source_track_id,
            track.source_time_bits,
            track.turn_side_bits,
        );
        let cloud = clouds.entry(key).or_insert_with(|| Cloud {
            weight: 0.0,
            trials: BTreeMap::new(),
        });
        cloud.weight += track.probability;
        cloud.trials.entry(track.trial_id).or_default().push(track.piece);
    }
    let mut by_object: BTreeMap<u64, Vec<TrajectoryInput>> = BTreeMap::new();
    for ((object_id, _), traj) in intact {
        by_object.entry(object_id).or_default().push(traj);
    }
    for ((object_id, _, _, _, _), cloud) in clouds {
        let trials: Vec<Vec<PieceInput>> = cloud.trials.into_values().collect();
        by_object.entry(object_id).or_default().push(TrajectoryInput {
            weight: cloud.weight,
            trials,
        });
    }
    by_object
        .into_iter()
        .map(|(id, trajectories)| ObjectInput {
            name: names.remove(&id).unwrap_or_else(|| format!("Object {id}")),
            trajectories,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertical(lon: f64, lat: f64, mass: f64, radius: f64, speed: f64) -> PieceInput {
        PieceInput {
            lon,
            lat,
            mass_kg: mass,
            radius_m: radius,
            v_east: 0.0,
            v_north: 0.0,
            v_up: -speed,
        }
    }

    #[test]
    fn weights_off_unity_are_normalized_explicitly() {
        let (p, how) = resolve_trajectory_weights(&[2.0, 2.0]).unwrap();
        assert_eq!(how, WeightResolution::Normalized { sum: 4.0 });
        assert!((p[0] - 0.5).abs() < 1e-12);
        assert!((p[1] - 0.5).abs() < 1e-12);
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-12);

        let (p, how) = resolve_trajectory_weights(&[0.25, 0.75]).unwrap();
        assert_eq!(how, WeightResolution::Unchanged);
        assert!((p[0] - 0.25).abs() < 1e-12);
    }

    #[test]
    fn negative_or_zero_weights_are_rejected() {
        assert!(resolve_trajectory_weights(&[-0.1, 1.1]).is_err());
        assert!(resolve_trajectory_weights(&[0.0, 0.0]).is_err());
    }

    #[test]
    fn kinetic_energy_below_threshold_drops_the_piece() {
        let ke = kinetic_energy_j(2.0, 3.0);
        assert!((ke - 9.0).abs() < 1e-12);
        let params = VesselParams {
            ke_threshold_j: 10.0,
            a_proj_m2: 100.0,
            ..VesselParams::default()
        };
        let dropped = trajectory_risk(
            &TrajectoryInput {
                weight: 1.0,
                trials: vec![vec![vertical(-90.0, 29.0, 2.0, 0.2, 3.0)]],
            },
            &params,
            cell_area_m2(),
            8,
        );
        assert!(dropped.0.is_empty(), "KE 9 J is below 10 J");

        let kept_params = VesselParams {
            ke_threshold_j: 9.0,
            ..params
        };
        let kept = trajectory_risk(
            &TrajectoryInput {
                weight: 1.0,
                trials: vec![vec![vertical(-90.0, 29.0, 2.0, 0.2, 3.0)]],
            },
            &kept_params,
            cell_area_m2(),
            8,
        );
        assert_eq!(kept.0.len(), 1);
    }

    #[test]
    fn two_debris_in_one_cell_use_the_product_union() {
        let a_g = 1_000.0;
        let risk = trial_cell_risk([200.0, 300.0], a_g);
        let summed = 0.2 + 0.3;
        assert!((risk - 0.44).abs() < 1e-12, "{risk}");
        assert!((risk - summed).abs() > 0.01);

        let capped = trial_cell_risk([5_000.0], a_g);
        assert!((capped - 1.0).abs() < 1e-12);
    }

    #[test]
    fn two_trajectories_combine_by_probability() {
        let mut a = HashMap::new();
        a.insert(1, 0.4);
        let mut b = HashMap::new();
        b.insert(1, 0.1);
        let (probs, how) = resolve_trajectory_weights(&[1.0, 3.0]).unwrap();
        assert_eq!(how, WeightResolution::Normalized { sum: 4.0 });
        let field = combine_trajectories(&[(probs[0], a), (probs[1], b)]);
        assert!((field[&1] - 0.175).abs() < 1e-12, "{}", field[&1]);
    }

    #[test]
    fn two_objects_combine_by_the_outer_product() {
        let mut a = HashMap::new();
        a.insert(7, 0.2);
        let mut b = HashMap::new();
        b.insert(7, 0.5);
        let risk = combine_objects(&[a, b]);
        let summed = 0.2 + 0.5;
        assert!((risk[&7] - 0.6).abs() < 1e-12);
        assert!((risk[&7] - summed).abs() > 0.01);
    }

    #[test]
    fn vulnerability_area_uses_sin_gamma() {
        let gamma = 30.0_f64.to_radians();
        let a_perp = 10.0 + 4.0 * 1.0 + std::f64::consts::PI * 1.0;
        let a_v = vulnerability_area_m2(10.0, 4.0, 1.0, gamma);
        assert!((a_v - a_perp / gamma.sin()).abs() < 1e-9, "{a_v}");

        let grazing = vulnerability_area_m2(10.0, 4.0, 1.0, 0.0);
        assert!((grazing - a_perp / SIN_GAMMA_MIN).abs() < 1e-9);
        assert!(grazing.is_finite());
        assert!(grazing < a_perp / 1e-6);
    }

    #[test]
    fn h3_hit_cells_share_one_area() {
        let a = cell_index(-90.25, 29.10).unwrap();
        let b = cell_index(-88.40, 30.80).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.resolution(), H3_RES);
        assert_eq!(b.resolution(), H3_RES);
        assert!(!a.is_pentagon());
        assert_eq!(a.boundary().len(), 6);
        assert_eq!(b.boundary().len(), 6);
        let area = cell_area_m2();
        assert_eq!(area, H3_RES.area_m2());
        assert_eq!(area, H3_RES.area_m2());
        assert!(area > 700_000.0 && area < 800_000.0, "{area}");
        let used = [area, area];
        assert_eq!(used[0], used[1]);
    }

    #[test]
    fn boats_in_different_cells_split_collective_and_individual() {
        let boat_a = BoatQuery {
            id: 1,
            name: "A".into(),
            lon: -90.25,
            lat: 29.10,
            people: Some(10),
        };
        let boat_b = BoatQuery {
            id: 2,
            name: "B".into(),
            lon: -88.40,
            lat: 30.80,
            people: Some(3),
        };
        let cell_a = u64::from(cell_index(boat_a.lon, boat_a.lat).unwrap());
        let cell_b = u64::from(cell_index(boat_b.lon, boat_b.lat).unwrap());
        assert_ne!(cell_a, cell_b);
        let mut risk = HashMap::new();
        risk.insert(cell_a, 0.2);
        risk.insert(cell_b, 0.5);
        let (scores, collective) = score_boats(&risk, &[boat_a, boat_b]);
        assert!((collective - 0.35).abs() < 1e-12, "{collective}");
        assert!((collective - (scores[0].strike + scores[1].strike) / 2.0).abs() < 1e-12);
        assert!((scores[0].strike - 0.2).abs() < 1e-12);
        assert!((scores[1].strike - 0.5).abs() < 1e-12);
        assert!((scores[0].individual.unwrap() - 2.0).abs() < 1e-12);
        assert!((scores[1].individual.unwrap() - 1.5).abs() < 1e-12);
        assert!((scores[0].individual.unwrap() - scores[0].strike * 10.0).abs() < 1e-12);
        assert!((scores[1].individual.unwrap() - scores[1].strike * 3.0).abs() < 1e-12);
    }

    #[test]
    fn stub_field_matches_the_hand_formulas() {
        let a_g = cell_area_m2();
        let params = VesselParams {
            ke_threshold_j: 0.0,
            a_proj_m2: 0.2 * a_g,
            p_proj_m: 0.0,
            max_trials: 4,
            default_mass_kg: 10.0,
            default_radius_m: 0.0,
        };
        let place = (-90.25, 29.10);
        let cloud = vec![
            vertical(place.0, place.1, 10.0, 0.0, 50.0),
            vertical(place.0, place.1, 10.0, 0.0, 50.0),
        ];
        let result = compute_vessel_risk(
            &[ObjectInput {
                name: "Vehicle".into(),
                trajectories: vec![TrajectoryInput {
                    weight: 1.0,
                    trials: vec![cloud],
                }],
            }],
            &params,
            &[],
        )
        .unwrap();
        let cell = u64::from(cell_index(place.0, place.1).unwrap());
        let drawn = result.cells.iter().find(|c| {
            cell_index_from_h3(&c.h3).map(|idx| u64::from(idx) == cell).unwrap_or(false)
        });
        let risk = drawn.map(|c| c.risk).unwrap_or(0.0);
        assert!((risk - 0.36).abs() < 1e-6, "product union risk {risk}");
        assert!(!result.smoothed);
    }

    fn cell_index_from_h3(text: &str) -> Option<CellIndex> {
        text.parse().ok()
    }

    #[test]
    fn one_monte_carlo_trial_keeps_its_debris_together() {
        let piece = |id, trial| TrackPiece {
            object_id: 1,
            object_name: "Stage".into(),
            mode_id: 4,
            probability: 0.1,
            fragment: true,
            source_track_id: 9,
            source_time_bits: 0,
            turn_side_bits: 0,
            trial_id: trial,
            track_id: id,
            piece: vertical(-90.0, 29.0, 5.0, 0.2, 40.0),
        };
        let objects = objects_from_tracks(vec![piece(1, 1), piece(2, 1), piece(3, 2)]);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].trajectories.len(), 1, "same breakup is one trajectory");
        let trials = &objects[0].trajectories[0].trials;
        assert_eq!(trials.len(), 2);
        assert_eq!(trials[0].len(), 2, "pieces that share a trial stay in one cloud");
        assert_eq!(trials[1].len(), 1);
        assert!((objects[0].trajectories[0].weight - 0.3).abs() < 1e-12);
    }

    #[test]
    fn botev_bandwidth_tracks_the_sample_spread() {
        let tight: Vec<[f64; 2]> = (0..80)
            .map(|i| {
                let t = i as f64;
                [1_000.0 * (t * 0.17).sin(), 800.0 * (t * 0.11).cos()]
            })
            .collect();
        let wide: Vec<[f64; 2]> = tight.iter().map(|p| [p[0] * 4.0, p[1]]).collect();
        let a = botev_bandwidth(&tight).expect("tight bandwidth");
        let b = botev_bandwidth(&wide).expect("wide bandwidth");
        assert!(a.bandwidth_x > 50.0, "east {}", a.bandwidth_x);
        assert!(b.bandwidth_x > a.bandwidth_x * 2.0, "{} vs {}", b.bandwidth_x, a.bandwidth_x);
        assert!((b.bandwidth_y - a.bandwidth_y).abs() / a.bandwidth_y < 0.35, "{} vs {}", b.bandwidth_y, a.bandwidth_y);
    }

    #[test]
    fn dct_roundtrip_on_a_small_grid() {
        let n = 8;
        let mut data = vec![0.0; n * n];
        for i in 0..data.len() {
            data[i] = ((i * 3) % 7) as f64 - 2.0;
        }
        let back = idct2d(&dct2d(&data, n), n);
        for (a, b) in data.iter().zip(back) {
            assert!((a - b).abs() < 1e-8, "{a} vs {b}");
        }
    }
}
