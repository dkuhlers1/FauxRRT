//! Spawn a local Python + RocketPy process and ingest the resulting LLA track.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::parse::ParsedTrack;
use crate::schema::DetectedSchema;
use crate::wind::WindSpec;

const FLY_PY: &str = include_str!("../../rocketpy_backend/fly.py");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RocketPySpec {
    #[serde(default)]
    pub env: RpEnv,
    #[serde(default)]
    pub motor: RpMotor,
    #[serde(default)]
    pub rocket: RpRocket,
    #[serde(default)]
    pub flight: RpFlight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpEnv {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub elevation_m: f64,
    #[serde(default = "standard_atmosphere")]
    pub atmosphere: String,
    #[serde(default)]
    pub wind_speed_mps: f64,
    #[serde(default)]
    pub wind_from_deg: f64,
}

fn standard_atmosphere() -> String {
    "standard_atmosphere".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpMotor {
    pub thrust_n: f64,
    pub burn_time_s: f64,
    pub dry_mass_kg: f64,
    pub propellant_mass_kg: f64,
    pub dry_inertia: [f64; 3],
    pub nozzle_radius_m: f64,
    pub chamber_radius_m: f64,
    pub chamber_height_m: f64,
    #[serde(default)]
    pub nozzle_position_m: f64,
    #[serde(default)]
    pub chamber_position_m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpRocket {
    pub radius_m: f64,
    pub mass_kg: f64,
    pub inertia: [f64; 3],
    pub power_off_cd: f64,
    pub power_on_cd: f64,
    #[serde(default)]
    pub center_of_mass_m: f64,
    pub motor_position_m: f64,
    pub nose_length_m: f64,
    #[serde(default = "von_karman")]
    pub nose_kind: String,
    pub nose_position_m: f64,
    pub fin_n: u32,
    pub fin_root_m: f64,
    pub fin_tip_m: f64,
    pub fin_span_m: f64,
    pub fin_position_m: f64,
    pub tail_top_m: f64,
    pub tail_bottom_m: f64,
    pub tail_length_m: f64,
    pub tail_position_m: f64,
    pub rail_upper_m: f64,
    pub rail_lower_m: f64,
}

fn von_karman() -> String {
    "von karman".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpFlight {
    pub rail_length_m: f64,
    pub inclination_deg: f64,
    pub heading_deg: f64,
    #[serde(default = "default_max_time")]
    pub max_time_s: f64,
}

fn default_max_time() -> f64 {
    400.0
}

impl Default for RpEnv {
    fn default() -> Self {
        Self {
            latitude: 32.990254,
            longitude: -106.974998,
            elevation_m: 1400.0,
            atmosphere: standard_atmosphere(),
            wind_speed_mps: 0.0,
            wind_from_deg: 270.0,
        }
    }
}

impl Default for RpMotor {
    fn default() -> Self {
        Self {
            thrust_n: 1500.0,
            burn_time_s: 3.9,
            dry_mass_kg: 1.815,
            propellant_mass_kg: 2.5,
            dry_inertia: [0.125, 0.125, 0.002],
            nozzle_radius_m: 0.033,
            chamber_radius_m: 0.033,
            chamber_height_m: 0.6,
            nozzle_position_m: 0.0,
            chamber_position_m: 0.0,
        }
    }
}

impl Default for RpRocket {
    fn default() -> Self {
        Self {
            radius_m: 0.0635,
            mass_kg: 14.426,
            inertia: [6.321, 6.321, 0.034],
            power_off_cd: 0.5,
            power_on_cd: 0.5,
            center_of_mass_m: 0.0,
            motor_position_m: -1.255,
            nose_length_m: 0.55829,
            nose_kind: von_karman(),
            nose_position_m: 1.278,
            fin_n: 4,
            fin_root_m: 0.12,
            fin_tip_m: 0.06,
            fin_span_m: 0.11,
            fin_position_m: -1.04956,
            tail_top_m: 0.0635,
            tail_bottom_m: 0.0435,
            tail_length_m: 0.06,
            tail_position_m: -1.194656,
            rail_upper_m: 0.0818,
            rail_lower_m: -0.618,
        }
    }
}

impl Default for RpFlight {
    fn default() -> Self {
        Self {
            rail_length_m: 5.2,
            inclination_deg: 85.0,
            heading_deg: 0.0,
            max_time_s: default_max_time(),
        }
    }
}

impl Default for RocketPySpec {
    fn default() -> Self {
        Self::calisto()
    }
}

impl RocketPySpec {
    pub fn calisto() -> Self {
        Self {
            env: RpEnv::default(),
            motor: RpMotor::default(),
            rocket: RpRocket::default(),
            flight: RpFlight::default(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(-90.0..=90.0).contains(&self.env.latitude) {
            return Err("launch latitude must be between -90 and 90".into());
        }
        if !(-180.0..=180.0).contains(&self.env.longitude) {
            return Err("launch longitude must be between -180 and 180".into());
        }
        if self.motor.thrust_n <= 0.0 {
            return Err("motor thrust must be positive".into());
        }
        if self.motor.burn_time_s <= 0.0 {
            return Err("motor burn time must be positive".into());
        }
        if self.motor.dry_mass_kg <= 0.0 || self.motor.propellant_mass_kg <= 0.0 {
            return Err("motor dry and propellant mass must be positive".into());
        }
        if self.rocket.radius_m <= 0.0 || self.rocket.mass_kg <= 0.0 {
            return Err("rocket radius and mass must be positive".into());
        }
        if !(3..=8).contains(&self.rocket.fin_n) {
            return Err("fin count must be 3 to 8".into());
        }
        if self.flight.rail_length_m <= 0.0 {
            return Err("rail length must be positive".into());
        }
        if !(-180.0..=180.0).contains(&self.flight.inclination_deg) {
            return Err("rail inclination must be between -180 and 180".into());
        }
        if self.flight.max_time_s < 1.0 || self.flight.max_time_s > 2_000.0 {
            return Err("max flight time must be between 1 and 2000 s".into());
        }
        Ok(())
    }

    pub fn with_mission_wind(mut self, wind: &WindSpec) -> Self {
        if let WindSpec::Constant {
            speed_mps,
            from_deg,
        } = wind
        {
            self.env.wind_speed_mps = *speed_mps;
            self.env.wind_from_deg = *from_deg;
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RocketPyStatus {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RocketPyFlight {
    pub parsed: ParsedTrack,
    pub apogee_m: Option<f64>,
    pub impact_time_s: Option<f64>,
}

#[derive(Deserialize)]
struct FlyOut {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    times: Option<Vec<f64>>,
    #[serde(default)]
    lat: Option<Vec<f64>>,
    #[serde(default)]
    lon: Option<Vec<f64>>,
    #[serde(default)]
    alt: Option<Vec<f64>>,
    #[serde(default)]
    apogee_m: Option<f64>,
    #[serde(default)]
    impact_time_s: Option<f64>,
}

#[derive(Clone)]
struct PyInterp {
    bin: PathBuf,
    prefix: Vec<String>,
    label: String,
}

static CHOSEN: Mutex<Option<(PyInterp, String)>> = Mutex::new(None);

fn rocketpy_backend_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("rocketpy_backend");
    let mut cands = vec![manifest];
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd.join("rocketpy_backend"));
        cands.push(cwd.join("..").join("rocketpy_backend"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("rocketpy_backend"));
            cands.push(dir.join("..").join("rocketpy_backend"));
            cands.push(dir.join("..").join("..").join("rocketpy_backend"));
        }
    }
    for cand in cands {
        if cand.join("fly.py").is_file() || cand.join(".venv").is_dir() {
            return fs::canonicalize(&cand).unwrap_or(cand);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("rocketpy_backend")
}

fn venv_dir() -> PathBuf {
    rocketpy_backend_dir().join(".venv")
}

fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn prepare_script() -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!("fauxrrt_rocketpy_{}.py", std::process::id()));
    fs::write(&path, FLY_PY).map_err(|e| format!("write RocketPy helper: {e}"))?;
    Ok(path)
}

fn apply_no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

fn spawn_python(
    interp: &PyInterp,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<(String, String, i32), String> {
    let mut cmd = Command::new(&interp.bin);
    cmd.args(&interp.prefix)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1")
        .env("MPLBACKEND", "Agg");
    apply_no_window(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", interp.label))?;
    if let Some(bytes) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(bytes)
                .map_err(|e| format!("write RocketPy stdin: {e}"))?;
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait for Python: {e}"))?;
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    ))
}

fn system_interps() -> Vec<PyInterp> {
    let mut out = Vec::new();
    if let Ok(raw) = std::env::var("FAUXRRT_PYTHON") {
        let path = PathBuf::from(raw);
        out.push(PyInterp {
            label: path.display().to_string(),
            bin: path,
            prefix: Vec::new(),
        });
    }
    if cfg!(windows) {
        out.push(PyInterp {
            bin: PathBuf::from("py"),
            prefix: vec!["-3".into()],
            label: "py -3".into(),
        });
        out.push(PyInterp {
            bin: PathBuf::from("python"),
            prefix: Vec::new(),
            label: "python".into(),
        });
    } else {
        out.push(PyInterp {
            bin: PathBuf::from("python3"),
            prefix: Vec::new(),
            label: "python3".into(),
        });
    }
    out.push(PyInterp {
        bin: PathBuf::from("python3"),
        prefix: Vec::new(),
        label: "python3".into(),
    });
    out.push(PyInterp {
        bin: PathBuf::from("python"),
        prefix: Vec::new(),
        label: "python".into(),
    });
    out
}

#[derive(Deserialize)]
struct Probe {
    ok: bool,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

fn parse_probe(stdout: &str, stderr: &str) -> Result<String, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim().starts_with('{'))
        .unwrap_or(stdout.trim());
    match serde_json::from_str::<Probe>(line) {
        Ok(p) if p.ok => Ok(p.version.unwrap_or_else(|| "unknown".into())),
        Ok(p) => Err(p
            .error
            .unwrap_or_else(|| "rocketpy is not installed".into())),
        Err(_) => Err(stderr
            .lines()
            .last()
            .unwrap_or("could not import rocketpy")
            .to_string()),
    }
}

fn probe_rocketpy(interp: &PyInterp) -> Result<String, String> {
    let script = prepare_script()?;
    let script_s = script.to_string_lossy().into_owned();
    let result = spawn_python(interp, &["-u", &script_s, "--check"], None);
    let _ = fs::remove_file(&script);
    let (stdout, stderr, _) = result?;
    parse_probe(&stdout, &stderr)
}

fn interp_from_venv() -> Option<PyInterp> {
    let py = venv_python(&venv_dir());
    py.is_file().then(|| PyInterp {
        label: format!("venv ({})", py.display()),
        bin: py,
        prefix: Vec::new(),
    })
}

fn first_working_system() -> Result<PyInterp, String> {
    let mut last = "no Python interpreter found".to_string();
    for interp in system_interps() {
        match spawn_python(&interp, &["-c", "print(1)"], None) {
            Ok((stdout, _, 0)) if stdout.contains('1') => return Ok(interp),
            Ok((_, stderr, _)) => {
                last = stderr.lines().last().unwrap_or(&last).to_string();
            }
            Err(err) => last = err,
        }
    }
    Err(format!(
        "{last}. Install Python 3, then reopen the RocketPy builder."
    ))
}

fn pip_install(interp: &PyInterp) -> Result<(), String> {
    let req = rocketpy_backend_dir().join("requirements.txt");
    let req_s = req.to_string_lossy().into_owned();
    let mut args = vec![
        "-m",
        "pip",
        "install",
        "--disable-pip-version-check",
        "--upgrade",
    ];
    if req.is_file() {
        args.push("-r");
        args.push(req_s.as_str());
    } else {
        args.push("rocketpy");
    }
    let (stdout, stderr, code) = spawn_python(interp, &args, None)?;
    if code == 0 {
        return Ok(());
    }
    let hint = stderr
        .lines()
        .chain(stdout.lines())
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("pip failed");
    Err(format!("pip install rocketpy failed: {hint}"))
}

fn bootstrap_venv() -> Result<PyInterp, String> {
    let creator = first_working_system()?;
    let venv = venv_dir();
    if let Some(parent) = venv.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create RocketPy venv folder: {e}"))?;
    }
    let py = venv_python(&venv);
    if !py.is_file() {
        let venv_s = venv.to_string_lossy().into_owned();
        let (stdout, stderr, code) = spawn_python(&creator, &["-m", "venv", &venv_s], None)?;
        if !py.is_file() || code != 0 {
            let hint = stderr
                .lines()
                .last()
                .or(stdout.lines().last())
                .unwrap_or("venv failed");
            return Err(format!("could not create Python venv: {hint}"));
        }
    }
    let interp = PyInterp {
        label: format!("venv ({})", py.display()),
        bin: py,
        prefix: Vec::new(),
    };
    pip_install(&interp)?;
    Ok(interp)
}

fn remember(interp: PyInterp, version: String) -> (PyInterp, String) {
    if let Ok(mut slot) = CHOSEN.lock() {
        *slot = Some((interp.clone(), version.clone()));
    }
    (interp, version)
}

fn ensure_interp() -> Result<(PyInterp, String), String> {
    if let Ok(slot) = CHOSEN.lock() {
        if let Some(pair) = slot.clone() {
            return Ok(pair);
        }
    }
    if let Some(interp) = interp_from_venv() {
        if let Ok(version) = probe_rocketpy(&interp) {
            return Ok(remember(interp, version));
        }
        if pip_install(&interp).is_ok() {
            if let Ok(version) = probe_rocketpy(&interp) {
                return Ok(remember(interp, version));
            }
        }
    }
    for interp in system_interps() {
        if let Ok(version) = probe_rocketpy(&interp) {
            return Ok(remember(interp, version));
        }
    }
    let interp = bootstrap_venv()?;
    let version = probe_rocketpy(&interp)?;
    Ok(remember(interp, version))
}

pub fn check_rocketpy() -> RocketPyStatus {
    match ensure_interp() {
        Ok((interp, version)) => RocketPyStatus {
            available: true,
            version: Some(version),
            python: Some(interp.label),
            error: None,
        },
        Err(err) => RocketPyStatus {
            available: false,
            version: None,
            python: None,
            error: Some(err),
        },
    }
}

pub fn fly_rocketpy(spec: &RocketPySpec) -> Result<RocketPyFlight, String> {
    spec.validate()?;
    let (interp, _) = ensure_interp()?;
    let script = prepare_script()?;
    let payload = serde_json::to_vec(spec).map_err(|e| format!("encode RocketPy spec: {e}"))?;
    let script_s = script.to_string_lossy().into_owned();
    let (stdout, stderr, _) = spawn_python(&interp, &["-u", &script_s], Some(&payload))?;
    let _ = fs::remove_file(&script);
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim().starts_with('{'))
        .unwrap_or(stdout.trim());
    let out: FlyOut = serde_json::from_str(line).map_err(|e| {
        let hint = stderr.lines().last().unwrap_or("no Python error text");
        format!("RocketPy output was not JSON ({e}). {hint}")
    })?;
    if !out.ok {
        return Err(out
            .error
            .unwrap_or_else(|| "RocketPy flight failed".into()));
    }
    let times = out.times.unwrap_or_default();
    let lat = out.lat.unwrap_or_default();
    let lon = out.lon.unwrap_or_default();
    let alt = out.alt.unwrap_or_default();
    let n = times.len().min(lat.len()).min(lon.len()).min(alt.len());
    if n < 2 {
        return Err("RocketPy returned fewer than two trajectory states".into());
    }
    let mut lla = Vec::with_capacity(n * 3);
    for i in 0..n {
        if !(-90.0..=90.0).contains(&lat[i]) || !(-180.0..=180.0).contains(&lon[i]) {
            continue;
        }
        lla.push(lon[i] as f32);
        lla.push(lat[i] as f32);
        lla.push(alt[i] as f32);
    }
    if lla.len() < 6 {
        return Err("RocketPy states were not valid lat/lon".into());
    }
    let kept = lla.len() / 3;
    Ok(RocketPyFlight {
        parsed: ParsedTrack {
            schema: DetectedSchema::generated(),
            times: Some(times.into_iter().take(kept).collect()),
            lla,
        },
        apogee_m: out.apogee_m,
        impact_time_s: out.impact_time_s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calisto_spec_validates() {
        RocketPySpec::calisto().validate().unwrap();
    }

    #[test]
    fn rejects_bad_latitude() {
        let mut spec = RocketPySpec::calisto();
        spec.env.latitude = 120.0;
        assert!(spec.validate().is_err());
    }

    #[test]
    fn mission_constant_wind_overrides_env() {
        let spec = RocketPySpec::calisto().with_mission_wind(&WindSpec::Constant {
            speed_mps: 12.0,
            from_deg: 90.0,
        });
        assert_eq!(spec.env.wind_speed_mps, 12.0);
        assert_eq!(spec.env.wind_from_deg, 90.0);
    }

    #[test]
    fn fly_script_is_embedded() {
        assert!(FLY_PY.contains("def fly("));
        assert!(FLY_PY.contains("GenericMotor"));
    }

    #[test]
    fn backend_dir_has_fly_script() {
        let dir = rocketpy_backend_dir();
        assert!(
            dir.join("fly.py").is_file(),
            "missing fly.py in {}",
            dir.display()
        );
    }
}
