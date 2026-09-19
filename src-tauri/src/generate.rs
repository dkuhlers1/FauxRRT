//! 3DOF ballistic generator: US-1976 atmosphere, J2 gravity, ECEF dynamics.
//!
//! Point-mass EOM in ECEF (RK4):
//!   ṙ = v
//!   v̇ = a_J2(r) − 2ω×v − ω×(ω×r) + a_drag(r,v,wind) + a_cmd
//! Drag is ½ρ|v_rel|v_rel/β with β = m/(C_d A), constant C_d, no lift.
//! A nav-fail command is an n·g₀ load-factor turn: acceleration perpendicular
//! to velocity, left of the current horizontal heading (east if vertical).

use serde::{Deserialize, Serialize};

use crate::geodesy::{ecef_to_lla, lla_to_ecef};
use crate::wind::{resolve_wind_spec, wind_ecef, WindSpec};
use crate::parse::ParsedTrack;
use crate::schema::DetectedSchema;

const MU: f64 = 3.986004418e14;
const J2: f64 = 1.082626683e-3;
const RE: f64 = 6_378_137.0;
const OMEGA: f64 = 7.2921151467e-5;
const G0: f64 = 9.80665;
const R_AIR: f64 = 287.05287;
const RE_GEO: f64 = 6_356_766.0;

fn default_burnout() -> f64 {
    80_000.0
}

fn default_failures() -> u32 {
    8
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateSpec {
    pub launch_lat: f64,
    pub launch_lon: f64,
    #[serde(default)]
    pub launch_alt_m: f64,
    pub aim_lat: f64,
    pub aim_lon: f64,
    #[serde(default)]
    pub aim_alt_m: f64,
    pub ballistic_coeff: f64,
    #[serde(default = "default_burnout")]
    pub burnout_alt_m: f64,
    #[serde(default = "default_failures")]
    pub failure_count: u32,
    #[serde(default, skip_serializing_if = "WindSpec::is_off")]
    pub wind: WindSpec,
}

impl GenerateSpec {
    pub fn without_wind(mut self) -> Self {
        self.wind = WindSpec::Off;
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExtraAccel {
    pub turn_g: f64,
    pub side: f64,
    pub sustain_speed: bool,
    /// Initial left-of-heading (or east when the flight is vertical).
    pub axis: Vec3,
    /// When true, rebuild left = up × v_h every step (coordinated load-factor turn).
    /// Vertical burns lock `axis` so left/right stay opposite.
    pub follow_velocity: bool,
}

impl ExtraAccel {
    pub fn none() -> Self {
        Self {
            turn_g: 0.0,
            side: 0.0,
            sustain_speed: false,
            axis: Vec3::new(0.0, 0.0, 0.0),
            follow_velocity: false,
        }
    }

    pub fn horizontal_turn_at(r: Vec3, v: Vec3, turn_g: f64, side: f64, sustain_speed: bool) -> Self {
        let (lon, lat, _) = ecef_to_lla(r.x, r.y, r.z);
        let (east, _north, up) = enu_basis(lat, lon);
        let v_h = v.sub(up.scale(v.dot(up)));
        let follow_velocity = v_h.norm() >= 1.0;
        let axis = if follow_velocity {
            up.cross(v_h).normalized()
        } else {
            east
        };
        Self {
            turn_g,
            side,
            sustain_speed,
            axis,
            follow_velocity,
        }
    }

    fn active(self) -> bool {
        self.turn_g > 0.0 && self.side.abs() > 0.0 && self.axis.norm() > 0.5
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub(crate) fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
    pub(crate) fn from_lla(lat: f64, lon: f64, alt: f64) -> Self {
        let (x, y, z) = lla_to_ecef(lat, lon, alt);
        Self { x, y, z }
    }
    pub(crate) fn from_ecef(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
    pub(crate) fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }
    pub(crate) fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub(crate) fn norm(self) -> f64 {
        self.dot(self).sqrt()
    }
    pub(crate) fn normalized(self) -> Self {
        let n = self.norm();
        if n < 1e-18 {
            Self::new(0.0, 0.0, 0.0)
        } else {
            self.scale(1.0 / n)
        }
    }
    pub(crate) fn scale(self, s: f64) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
    pub(crate) fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
    pub(crate) fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

fn omega_cross(r: Vec3) -> Vec3 {
    Vec3::new(-OMEGA * r.y, OMEGA * r.x, 0.0)
}

fn rotate_z(r: Vec3, angle: f64) -> Vec3 {
    let c = angle.cos();
    let s = angle.sin();
    Vec3::new(c * r.x - s * r.y, s * r.x + c * r.y, r.z)
}

fn rotate_about(v: Vec3, axis: Vec3, angle: f64) -> Vec3 {
    let k = axis.normalized();
    let c = angle.cos();
    let s = angle.sin();
    v.scale(c)
        .add(k.cross(v).scale(s))
        .add(k.scale(k.dot(v) * (1.0 - c)))
}

pub fn resolve_winds(mut spec: GenerateSpec) -> Result<GenerateSpec, String> {
    spec.wind = resolve_wind_spec(
        spec.wind,
        spec.launch_lat,
        spec.launch_lon,
        spec.aim_lat,
        spec.aim_lon,
    )?;
    Ok(spec)
}

pub fn generate_track(spec: &GenerateSpec) -> Result<ParsedTrack, String> {
    validate(spec)?;
    let (v0, _gamma, _az) = min_energy_burnout(spec);
    let v_nom = shoot_velocity(spec, v0);
    Ok(integrate_named("Generated", spec, spec.ballistic_coeff, v_nom)?.1)
}

fn validate(spec: &GenerateSpec) -> Result<(), String> {
    if !(-90.0..=90.0).contains(&spec.launch_lat) || !(-90.0..=90.0).contains(&spec.aim_lat) {
        return Err("latitude must be between -90 and 90".into());
    }
    if spec.ballistic_coeff < 5.0 || spec.ballistic_coeff > 50_000.0 {
        return Err("ballistic coefficient should be 5–50000 kg/m²".into());
    }
    if let WindSpec::Constant { speed_mps, from_deg } = spec.wind {
        if !(0.0..=150.0).contains(&speed_mps) {
            return Err("wind speed should be 0–150 m/s".into());
        }
        if !from_deg.is_finite() {
            return Err("wind direction is invalid".into());
        }
    }
    let launch = Vec3::from_lla(
        spec.launch_lat,
        spec.launch_lon,
        spec.launch_alt_m + spec.burnout_alt_m.max(0.0),
    );
    let aim = Vec3::from_lla(spec.aim_lat, spec.aim_lon, spec.aim_alt_m);
    if launch.sub(aim).norm() < 2_000.0 {
        return Err("launch and aimpoint are too close".into());
    }
    Ok(())
}

fn min_energy_burnout(spec: &GenerateSpec) -> (Vec3, f64, f64) {
    let r1 = Vec3::from_lla(
        spec.launch_lat,
        spec.launch_lon,
        spec.launch_alt_m + spec.burnout_alt_m.max(0.0),
    );
    let r2 = Vec3::from_lla(spec.aim_lat, spec.aim_lon, spec.aim_alt_m);
    let mut tof = (r2.sub(r1).norm() / 3_200.0).clamp(60.0, 3_500.0);
    let mut v_eci = min_energy_velocity(r1, r2);
    for _ in 0..5 {
        let r2i = rotate_z(r2, OMEGA * tof);
        v_eci = min_energy_velocity(r1, r2i);
        tof = transfer_tof(r1, r2i, v_eci);
    }
    let v = v_eci.sub(omega_cross(r1));
    let (east, north, up) = enu_basis(spec.launch_lat, spec.launch_lon);
    let ve = v.dot(east);
    let vn = v.dot(north);
    let vu = v.dot(up);
    let horiz = ve.hypot(vn);
    let gamma = vu.atan2(horiz);
    let az = ve.atan2(vn);
    (v, gamma, az)
}

fn min_energy_velocity(r1: Vec3, r2: Vec3) -> Vec3 {
    let r1n = r1.norm();
    let r2n = r2.norm();
    let chord = r2.sub(r1);
    let c = chord.norm().max(1.0);
    let s = 0.5 * (r1n + r2n + c);
    let a = (s * 0.5).max(r1n * 0.51);
    let d1 = (s - r1n).clamp(0.0, c);
    let fstar = r1.add(chord.scale(d1 / c));
    let energy = (MU * (2.0 / r1n - 1.0 / a)).max(0.0);
    let speed = energy.sqrt();
    let to_focus = r1.scale(-1.0).normalized();
    let to_vacant = fstar.sub(r1).normalized();
    let normal = to_focus.add(to_vacant);
    let plane = r1.cross(r2);
    let mut tangent = normal.cross(plane);
    if tangent.norm() < 1e-8 {
        tangent = r1.cross(plane);
    }
    if tangent.dot(chord) < 0.0 {
        tangent = tangent.scale(-1.0);
    }
    tangent.normalized().scale(speed)
}

/// Lagrange TOF on the short-way ellipse through r1 → r2.
fn transfer_tof(r1: Vec3, r2: Vec3, v1: Vec3) -> f64 {
    let r1n = r1.norm();
    let r2n = r2.norm();
    let c = r2.sub(r1).norm().max(1.0);
    let s = 0.5 * (r1n + r2n + c);
    let energy = 0.5 * v1.dot(v1) - MU / r1n;
    let a = if energy < -1.0 {
        (-MU / (2.0 * energy)).max(s * 0.5 + 1.0)
    } else {
        s.max(r1n)
    };
    let x = (s / (2.0 * a)).clamp(0.0, 1.0).sqrt();
    let y = ((s - c) / (2.0 * a)).clamp(0.0, 1.0).sqrt();
    let alpha = 2.0 * x.asin();
    let beta = 2.0 * y.asin();
    let tof = (a.powi(3) / MU).sqrt() * ((alpha - alpha.sin()) - (beta - beta.sin()));
    tof.clamp(30.0, 4_500.0)
}

fn local_horizontal_basis(launch: Vec3, aim: Vec3) -> (Vec3, Vec3, Vec3) {
    let up = launch.normalized();
    let chord = aim.sub(launch);
    let mut along = chord.sub(up.scale(chord.dot(up)));
    if along.norm() < 1.0 {
        along = aim.sub(up.scale(aim.dot(up)));
    }
    let along = along.normalized();
    let side = up.cross(along).normalized();
    (up, along, side)
}

fn shoot_velocity(spec: &GenerateSpec, mut v: Vec3) -> Vec3 {
    let aim = Vec3::from_lla(spec.aim_lat, spec.aim_lon, spec.aim_alt_m);
    let launch = Vec3::from_lla(
        spec.launch_lat,
        spec.launch_lon,
        spec.launch_alt_m + spec.burnout_alt_m.max(0.0),
    );
    let range = aim.sub(launch).norm().max(1.0);
    let (up, along, side) = local_horizontal_basis(launch, aim);
    for _ in 0..12 {
        match propagate(spec, spec.ballistic_coeff, v, false) {
            Ok(p) => {
                let miss = p.impact.sub(aim);
                if miss.norm() < 250.0 {
                    break;
                }
                let along_miss = miss.dot(along);
                let side_miss = miss.dot(side);
                v = v.scale((1.0 - 0.5 * along_miss / range).clamp(0.88, 1.14));
                let yaw = (-side_miss / range).clamp(-0.035, 0.035);
                v = rotate_about(v, up, yaw);
            }
            Err(_) => v = v.scale(0.93),
        }
    }
    v
}

fn integrate_named(
    name: &str,
    spec: &GenerateSpec,
    beta: f64,
    v: Vec3,
) -> Result<(String, ParsedTrack), String> {
    let (times, lla, _) = integrate(spec, beta, v)?;
    Ok((
        name.to_string(),
        ParsedTrack {
            schema: DetectedSchema::generated(),
            times: Some(times),
            lla,
        },
    ))
}

pub(crate) struct Propagated {
    pub times: Vec<f64>,
    pub lla: Vec<f32>,
    pub impact: Vec3,
    pub r: Vec3,
    pub v: Vec3,
}

fn integrate(spec: &GenerateSpec, beta: f64, v0: Vec3) -> Result<(Vec<f64>, Vec<f32>, Vec3), String> {
    let mut p = propagate(spec, beta, v0, true)?;
    downsample(&mut p.times, &mut p.lla, 360);
    if p.lla.len() < 6 {
        return Err("generated trajectory was too short".into());
    }
    Ok((p.times, p.lla, p.impact))
}

pub(crate) fn track_from_state(
    r0: Vec3,
    v0: Vec3,
    beta: f64,
    wind: &WindSpec,
    ground_alt_m: f64,
    time_offset: f64,
) -> Result<ParsedTrack, String> {
    let mut p = propagate_from(
        r0,
        v0,
        beta,
        wind,
        ground_alt_m,
        None,
        ExtraAccel::none(),
        true,
    )?;
    downsample(&mut p.times, &mut p.lla, 360);
    if p.lla.len() < 6 {
        return Err("propagated trajectory was too short".into());
    }
    if time_offset.abs() > 1e-12 {
        for t in &mut p.times {
            *t += time_offset;
        }
    }
    Ok(ParsedTrack {
        schema: DetectedSchema::generated(),
        times: Some(p.times),
        lla: p.lla,
    })
}

pub(crate) fn integrate_turn(
    r0: Vec3,
    v0: Vec3,
    beta: f64,
    wind: &WindSpec,
    duration_s: f64,
    max_g: f64,
    side: f64,
    sustain_speed: bool,
    time_offset: f64,
) -> Result<(ParsedTrack, Vec3, Vec3), String> {
    if duration_s <= 0.0 || duration_s > 120.0 {
        return Err("turn duration should be between 0 and 120 s".into());
    }
    if !(0.1..=20.0).contains(&max_g) {
        return Err("max g should be 0.1–20".into());
    }
    let extra = ExtraAccel::horizontal_turn_at(r0, v0, max_g, side, sustain_speed);
    let mut p = propagate_from(r0, v0, beta, wind, -1.0e6, Some(duration_s), extra, true)?;
    if p.lla.len() < 6 {
        return Err("turn trajectory was too short".into());
    }
    if time_offset.abs() > 1e-12 {
        for t in &mut p.times {
            *t += time_offset;
        }
    }
    Ok((
        ParsedTrack {
            schema: DetectedSchema::generated(),
            times: Some(p.times),
            lla: p.lla,
        },
        p.r,
        p.v,
    ))
}

fn propagate(spec: &GenerateSpec, beta: f64, v0: Vec3, record: bool) -> Result<Propagated, String> {
    let r = Vec3::from_lla(
        spec.launch_lat,
        spec.launch_lon,
        spec.launch_alt_m + spec.burnout_alt_m.max(0.0),
    );
    propagate_from(
        r,
        v0,
        beta,
        &spec.wind,
        spec.aim_alt_m,
        None,
        ExtraAccel::none(),
        record,
    )
}

fn propagate_from(
    mut r: Vec3,
    mut v: Vec3,
    beta: f64,
    wind: &WindSpec,
    ground_alt_m: f64,
    max_time: Option<f64>,
    extra: ExtraAccel,
    record: bool,
) -> Result<Propagated, String> {
    let mut t = 0.0;
    let mut times = vec![0.0];
    let mut lla = Vec::new();
    if record {
        push_lla(&mut lla, r);
    }

    let mut last_record = 0.0;
    let mut impact = r;
    let turning = extra.active();
    let t_limit = max_time.unwrap_or(4_500.0).clamp(0.05, 4_500.0);

    for _ in 0..20_000 {
        let alt = altitude(r);
        let mut dt = step_dt(alt, v.norm(), ground_alt_m);
        if turning {
            dt = dt.min(0.05);
        }
        if let Some(t_end) = max_time {
            dt = dt.min((t_end - t).max(1e-4));
        }
        let (r1, v1) = rk4_step(r, v, dt, beta, wind, extra);
        let alt1 = altitude(r1);
        if max_time.is_none() && alt1 < alt && alt1 <= ground_alt_m + 2.0 {
            let denom = alt - alt1;
            let frac = if denom.abs() < 1e-9 {
                1.0
            } else {
                ((alt - ground_alt_m) / denom).clamp(0.0, 1.0)
            };
            let r_hit = r.add(r1.sub(r).scale(frac));
            let v_hit = v.add(v1.sub(v).scale(frac));
            t += dt * frac;
            if record {
                times.push(t);
                push_lla(&mut lla, r_hit);
            }
            return Ok(Propagated {
                times,
                lla,
                impact: r_hit,
                r: r_hit,
                v: v_hit,
            });
        }
        t += dt;
        r = r1;
        v = v1;
        if record && (turning || t - last_record >= record_interval(alt1)) {
            times.push(t);
            push_lla(&mut lla, r);
            last_record = t;
        }
        impact = r;
        if max_time.is_some() && t >= t_limit - 1e-9 {
            break;
        }
        if alt1 < -800.0 || t > 4_500.0 || r.norm() < RE * 0.7 {
            break;
        }
        if alt1 > 2.5e7 {
            return Err("trajectory escaped — check range and ballistic coefficient".into());
        }
    }

    Ok(Propagated {
        times,
        lla,
        impact,
        r,
        v,
    })
}

fn step_dt(alt: f64, speed: f64, aim_alt: f64) -> f64 {
    let speed = speed.max(50.0);
    let mut dt = (1_200.0 / speed).clamp(0.05, 2.0);
    if alt > 100_000.0 {
        dt = dt.clamp(0.5, 2.0);
    } else if alt < 40_000.0 {
        dt = dt.min(0.12);
    }
    if alt < 12_000.0 {
        let room = (alt - aim_alt).max(80.0);
        dt = dt.min((room / speed).clamp(0.02, 0.12));
    }
    dt
}

fn record_interval(alt: f64) -> f64 {
    if alt < 40_000.0 {
        0.25
    } else if alt < 80_000.0 {
        0.4
    } else {
        1.5
    }
}

fn rk4_step(r: Vec3, v: Vec3, dt: f64, beta: f64, wind: &WindSpec, extra: ExtraAccel) -> (Vec3, Vec3) {
    let a0 = accel(r, v, beta, wind, extra);
    let r1 = r.add(v.scale(0.5 * dt));
    let v1 = v.add(a0.scale(0.5 * dt));
    let a1 = accel(r1, v1, beta, wind, extra);
    let r2 = r.add(v1.scale(0.5 * dt));
    let v2 = v.add(a1.scale(0.5 * dt));
    let a2 = accel(r2, v2, beta, wind, extra);
    let r3 = r.add(v2.scale(dt));
    let v3 = v.add(a2.scale(dt));
    let a3 = accel(r3, v3, beta, wind, extra);
    let r_n = r.add(v.add(v1.scale(2.0)).add(v2.scale(2.0)).add(v3).scale(dt / 6.0));
    let v_n = v.add(a0.add(a1.scale(2.0)).add(a2.scale(2.0)).add(a3).scale(dt / 6.0));
    (r_n, v_n)
}

fn accel(r: Vec3, v: Vec3, beta: f64, wind: &WindSpec, extra: ExtraAccel) -> Vec3 {
    let r2 = r.dot(r);
    let rn = r2.sqrt();
    let zr = r.z / rn;
    let j2f = 1.5 * J2 * (RE / rn) * (RE / rn);
    let mu_r3 = -MU / (rn * r2);
    let mut ax = mu_r3 * r.x * (1.0 + j2f * (1.0 - 5.0 * zr * zr));
    let mut ay = mu_r3 * r.y * (1.0 + j2f * (1.0 - 5.0 * zr * zr));
    let mut az = mu_r3 * r.z * (1.0 + j2f * (3.0 - 5.0 * zr * zr));

    // ECEF: Coriolis −2Ω×v and centrifugal −Ω×(Ω×r)
    ax += 2.0 * OMEGA * v.y + OMEGA * OMEGA * r.x;
    ay += -2.0 * OMEGA * v.x + OMEGA * OMEGA * r.y;

    let (lon, lat, alt) = ecef_to_lla(r.x, r.y, r.z);
    let (wx, wy, wz) = wind_ecef(wind, lat, lon, alt);
    let vrel = Vec3::new(v.x - wx, v.y - wy, v.z - wz);
    let vm = vrel.norm();
    let rho = density_us76(alt);
    let mut drag = Vec3::new(0.0, 0.0, 0.0);
    if vm > 1.0 && rho > 1e-16 && beta > 1.0 {
        let k = -0.5 * rho * vm / beta;
        drag = Vec3::new(k * vrel.x, k * vrel.y, k * vrel.z);
        ax += drag.x;
        ay += drag.y;
        az += drag.z;
    }
    if extra.sustain_speed && extra.active() {
        let vhat = v.normalized();
        let along = drag.dot(vhat);
        ax -= along * vhat.x;
        ay -= along * vhat.y;
        az -= along * vhat.z;
    }
    if extra.active() {
        let (_east, _north, up) = enu_basis(lat, lon);
        let mut dir = extra.axis;
        if extra.follow_velocity {
            let v_h = v.sub(up.scale(v.dot(up)));
            let left = up.cross(v_h);
            if left.norm() >= 1.0 {
                dir = left.normalized();
            }
        }
        let mag = extra.side.signum() * extra.turn_g * G0;
        let mut at = dir.scale(mag);
        let speed = v.norm();
        if speed > 1.0 {
            let vhat = v.scale(1.0 / speed);
            at = at.sub(vhat.scale(at.dot(vhat)));
        }
        ax += at.x;
        ay += at.y;
        az += at.z;
    }
    Vec3::new(ax, ay, az)
}

pub(crate) fn ecef_coast_accel(r: Vec3, v: Vec3) -> Vec3 {
    accel(r, v, 1e12, &WindSpec::Off, ExtraAccel::none())
}

fn altitude(r: Vec3) -> f64 {
    ecef_to_lla(r.x, r.y, r.z).2
}

fn push_lla(out: &mut Vec<f32>, r: Vec3) {
    let (lon, lat, alt) = ecef_to_lla(r.x, r.y, r.z);
    out.push(lon as f32);
    out.push(lat as f32);
    out.push(alt as f32);
}

pub(crate) fn enu_basis(lat_deg: f64, lon_deg: f64) -> (Vec3, Vec3, Vec3) {
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();
    let sl = lat.sin();
    let cl = lat.cos();
    let so = lon.sin();
    let co = lon.cos();
    let east = Vec3::new(-so, co, 0.0);
    let north = Vec3::new(-sl * co, -sl * so, cl);
    let up = Vec3::new(cl * co, cl * so, sl);
    (east, north, up)
}

/// US Standard Atmosphere 1976 density (kg/m³) vs geometric altitude.
pub fn density_us76(alt_m: f64) -> f64 {
    if alt_m < -500.0 {
        return 1.4;
    }
    if alt_m > 1_000_000.0 {
        return 0.0;
    }
    let z = alt_m.max(-500.0);
    let h = RE_GEO * z / (RE_GEO + z);
    const H: [f64; 8] = [0.0, 11_000.0, 20_000.0, 32_000.0, 47_000.0, 51_000.0, 71_000.0, 84_852.0];
    const T: [f64; 8] = [288.15, 216.65, 216.65, 228.65, 270.65, 270.65, 214.65, 186.946];
    const L: [f64; 8] = [-0.0065, 0.0, 0.001, 0.0028, 0.0, -0.0028, -0.002, 0.0];
    let mut p = 101_325.0;
    let mut t = T[0];
    for i in 0..7 {
        let top = H[i + 1].min(h);
        let dh = top - H[i];
        if dh > 0.0 {
            if L[i].abs() < 1e-12 {
                p *= (-G0 * dh / (R_AIR * T[i])).exp();
                t = T[i];
            } else {
                t = T[i] + L[i] * dh;
                p *= (t / T[i]).powf(-G0 / (R_AIR * L[i]));
            }
        }
        if h <= H[i + 1] {
            return p / (R_AIR * t);
        }
    }
    let rho86 = p / (R_AIR * t);
    density_us76_high(z, rho86)
}

/// Official US-1976 number-density-derived mass densities above 86 km,
/// log-interpolated and scaled to match the hydrostatic value at 86 km.
fn density_us76_high(z: f64, rho86: f64) -> f64 {
    const Z: [f64; 16] = [
        86_000.0, 90_000.0, 95_000.0, 100_000.0, 110_000.0, 120_000.0, 150_000.0, 200_000.0,
        250_000.0, 300_000.0, 400_000.0, 500_000.0, 600_000.0, 700_000.0, 800_000.0, 1_000_000.0,
    ];
    const RHO: [f64; 16] = [
        6.958e-6, 3.416e-6, 1.393e-6, 5.604e-7, 9.708e-8, 2.222e-8, 2.075e-9, 2.541e-10, 6.073e-11,
        1.916e-11, 2.803e-12, 5.215e-13, 1.137e-13, 3.070e-14, 1.136e-14, 3.561e-15,
    ];
    let scale = rho86 / RHO[0];
    if z <= Z[0] {
        return rho86;
    }
    for i in 0..Z.len() - 1 {
        if z <= Z[i + 1] {
            let t = (z - Z[i]) / (Z[i + 1] - Z[i]);
            let log_rho = (RHO[i] * scale).ln() * (1.0 - t) + (RHO[i + 1] * scale).ln() * t;
            return log_rho.exp();
        }
    }
    RHO[RHO.len() - 1] * scale
}

fn downsample(times: &mut Vec<f64>, lla: &mut Vec<f32>, max_pts: usize) {
    let n = lla.len() / 3;
    if n <= max_pts {
        return;
    }
    let step = ((n as f64) / max_pts as f64).ceil() as usize;
    let mut t2 = Vec::new();
    let mut l2 = Vec::new();
    let mut last_i = 0;
    for i in (0..n).step_by(step.max(1)) {
        last_i = i;
        if i < times.len() {
            t2.push(times[i]);
        }
        l2.extend_from_slice(&lla[i * 3..i * 3 + 3]);
    }
    if last_i != n - 1 {
        t2.push(*times.last().unwrap());
        l2.extend_from_slice(&lla[(n - 1) * 3..]);
    }
    *times = t2;
    *lla = l2;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white_sands() -> GenerateSpec {
        GenerateSpec {
            launch_lat: 32.4,
            launch_lon: -106.4,
            launch_alt_m: 0.0,
            aim_lat: 34.0,
            aim_lon: -99.0,
            aim_alt_m: 0.0,
            ballistic_coeff: 2500.0,
            burnout_alt_m: 80_000.0,
            failure_count: 3,
            wind: WindSpec::Off,
        }
    }

    fn ground_az(lla: &[f32], i: usize) -> f64 {
        let lon0 = lla[(i - 1) * 3] as f64;
        let lat0 = lla[(i - 1) * 3 + 1] as f64;
        let lon = lla[i * 3] as f64;
        let lat = lla[i * 3 + 1] as f64;
        let dlon = (lon - lon0).to_radians();
        let dlat = (lat - lat0).to_radians();
        let mean_lat = ((lat + lat0) * 0.5).to_radians();
        let east = dlon * mean_lat.cos() * RE;
        let north = dlat * RE;
        east.atan2(north).to_degrees()
    }

    fn wrap_deg(d: f64) -> f64 {
        let mut x = d;
        if x > 180.0 {
            x -= 360.0;
        } else if x < -180.0 {
            x += 360.0;
        }
        x
    }

    #[test]
    fn sea_level_density_matches_standard() {
        let rho = density_us76(0.0);
        assert!((rho - 1.225).abs() < 0.02, "{rho}");
    }

    #[test]
    fn tropopause_density_is_thinner() {
        let rho = density_us76(11_000.0);
        assert!(rho > 0.3 && rho < 0.45, "{rho}");
    }

    #[test]
    fn density_above_86km_matches_us76_order() {
        let rho86 = density_us76(86_000.0);
        assert!((rho86 - 6.96e-6).abs() / 6.96e-6 < 0.08, "{rho86}");
        let rho100 = density_us76(100_000.0);
        assert!((rho100 - 5.60e-7).abs() / 5.60e-7 < 0.12, "{rho100}");
        let rho200 = density_us76(200_000.0);
        assert!((rho200 - 2.54e-10).abs() / 2.54e-10 < 0.2, "{rho200}");
    }

    #[test]
    fn min_energy_track_reaches_the_ground() {
        let spec = white_sands();
        let track = generate_track(&spec).expect("generate");
        assert!(track.lla.len() >= 9);
        let n = track.lla.len() / 3;
        let impact_alt = track.lla[n * 3 - 1];
        assert!(impact_alt.abs() < 80.0, "impact alt {impact_alt}");
    }

    #[test]
    fn nominal_lands_near_aimpoint() {
        let spec = white_sands();
        let track = generate_track(&spec).expect("generate");
        let lla = &track.lla;
        let n = lla.len() / 3;
        let impact = Vec3::from_lla(
            lla[n * 3 - 2] as f64,
            lla[n * 3 - 3] as f64,
            lla[n * 3 - 1] as f64,
        );
        let aim = Vec3::from_lla(spec.aim_lat, spec.aim_lon, spec.aim_alt_m);
        let miss = impact.sub(aim).norm();
        assert!(miss < 3_000.0, "aim miss {miss} m");
    }

    #[test]
    fn terminal_heading_does_not_hook() {
        let spec = GenerateSpec {
            failure_count: 0,
            ..white_sands()
        };
        let track = generate_track(&spec).expect("generate");
        let lla = &track.lla;
        let n = lla.len() / 3;
        let mut i20 = None;
        for i in 1..n {
            if lla[i * 3 + 2] < 20_000.0 {
                i20 = Some(i);
                break;
            }
        }
        let i20 = i20.expect("should descend through 20 km");
        let az20 = ground_az(lla, i20);
        let az_end = ground_az(lla, n - 1);
        let daz = wrap_deg(az_end - az20).abs();
        assert!(daz < 2.0, "terminal heading change {daz} deg (20 km → impact)");
    }

    #[test]
    fn drag_is_opposite_ecef_velocity() {
        let r = Vec3::from_lla(32.4, -106.4, 20_000.0);
        let v = Vec3::new(1_000.0, 2_000.0, -500.0);
        let a_coast = accel(r, v, 1e12, &WindSpec::Off, ExtraAccel::none());
        let a_drag = accel(r, v, 200.0, &WindSpec::Off, ExtraAccel::none());
        let extra = a_drag.sub(a_coast);
        assert!(extra.dot(v) < 0.0, "drag should oppose ECEF velocity");
        let aligned = extra.normalized().dot(v.normalized());
        assert!((aligned + 1.0).abs() < 1e-6, "drag direction {aligned}");
    }

    #[test]
    fn constant_wind_changes_air_relative_drag() {
        let r = Vec3::from_lla(32.4, -106.4, 10_000.0);
        let v = Vec3::new(1_200.0, 400.0, -200.0);
        let calm = accel(r, v, 200.0, &WindSpec::Off, ExtraAccel::none());
        let windy = accel(
            r,
            v,
            200.0,
            &WindSpec::Constant {
                speed_mps: 40.0,
                from_deg: 270.0,
            },
            ExtraAccel::none(),
        );
        assert!(calm.sub(windy).norm() > 1e-4);
        let coast = accel(r, v, 1e12, &WindSpec::Off, ExtraAccel::none());
        let extra = windy.sub(coast);
        let (lon, lat, alt) = ecef_to_lla(r.x, r.y, r.z);
        let (wx, wy, wz) = crate::wind::wind_ecef(
            &WindSpec::Constant {
                speed_mps: 40.0,
                from_deg: 270.0,
            },
            lat,
            lon,
            alt,
        );
        let vrel = Vec3::new(v.x - wx, v.y - wy, v.z - wz);
        let aligned = extra.normalized().dot(vrel.normalized());
        assert!((aligned + 1.0).abs() < 1e-5, "windy drag {aligned}");
    }

    #[test]
    fn vertical_nav_accel_yaws_east_west() {
        let r = Vec3::from_lla(32.4, -106.4, 10_000.0);
        let (east, _north, up) = enu_basis(32.4, -106.4);
        let v = up.scale(1_500.0);
        let coast = accel(r, v, 2_500.0, &WindSpec::Off, ExtraAccel::none());
        let left = accel(
            r,
            v,
            2_500.0,
            &WindSpec::Off,
            ExtraAccel::horizontal_turn_at(r, v, 5.0, 1.0, true),
        );
        let right = accel(
            r,
            v,
            2_500.0,
            &WindSpec::Off,
            ExtraAccel::horizontal_turn_at(r, v, 5.0, -1.0, true),
        );
        let d_l = left.sub(coast);
        let d_r = right.sub(coast);
        assert!(d_l.norm() > 20.0, "left extra accel {}", d_l.norm());
        assert!(d_l.dot(east) > 20.0, "left should yaw east {}", d_l.dot(east));
        assert!(d_r.dot(east) < -20.0, "right should yaw west {}", d_r.dot(east));
    }

    #[test]
    fn vertical_integrate_turn_splits_east_west() {
        let r = Vec3::from_lla(32.4, -106.4, 10_000.0);
        let (_east, _north, up) = enu_basis(32.4, -106.4);
        let v = up.scale(1_500.0);
        let (left, _, _) = integrate_turn(r, v, 2_500.0, &WindSpec::Off, 5.0, 5.0, 1.0, true, 0.0).unwrap();
        let (right, _, _) = integrate_turn(r, v, 2_500.0, &WindSpec::Off, 5.0, 5.0, -1.0, true, 0.0).unwrap();
        let end = |lla: &[f32]| {
            let n = lla.len() / 3;
            (lla[(n - 1) * 3] as f64, lla[(n - 1) * 3 + 1] as f64, lla[(n - 1) * 3 + 2] as f64, n)
        };
        let (lon_l, lat_l, alt_l, n_l) = end(&left.lla);
        let (lon_r, lat_r, alt_r, n_r) = end(&right.lla);
        let sep = (lon_l - lon_r).hypot(lat_l - lat_r);
        assert!(
            sep > 0.0005,
            "L {lon_l},{lat_l} alt={alt_l} n={n_l} R {lon_r},{lat_r} alt={alt_r} n={n_r}"
        );
    }

    #[test]
    fn coordinated_turn_changes_heading() {
        let r = Vec3::from_lla(32.4, -106.4, 40_000.0);
        let (east, north, up) = enu_basis(32.4, -106.4);
        let v = east.scale(1_200.0).add(up.scale(200.0));
        let extra = ExtraAccel::horizontal_turn_at(r, v, 5.0, 1.0, true);
        assert!(extra.follow_velocity, "airborne heading should follow velocity");
        let (_track, _rf, vf) = integrate_turn(r, v, 2_500.0, &WindSpec::Off, 5.0, 5.0, 1.0, true, 0.0).unwrap();
        let h0 = v.sub(up.scale(v.dot(up)));
        let hf = vf.sub(up.scale(vf.dot(up)));
        let cos = h0.normalized().dot(hf.normalized());
        assert!(cos < 0.995, "5 s at 5 g should yaw heading, cos={cos}");
        assert!(hf.dot(north) > 0.0, "left turn from eastbound should add north");
    }
}
