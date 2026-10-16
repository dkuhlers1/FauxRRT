//! Recover thrust and aerodynamic products from one loaded trajectory.
//!
//! The force fit lives in `rocketpy_backend/recover.py`. This module checks that
//! acceleration, orientation, and mass were actually in the file, converts the
//! stored ECEF states into the East-North-Up table that fitter reads, and runs it.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use crate::geodesy::{ecef_to_enu_vector, lla_to_ecef};

pub fn missing_parameter_message(groups: &[&str]) -> String {
    let order = ["acceleration", "orientation", "mass", "time", "position", "velocity"];
    let mut listed: Vec<&str> = order.iter().copied().filter(|name| groups.contains(name)).collect();
    if listed.is_empty() {
        listed = groups.to_vec();
    }
    let subject = match listed.as_slice() {
        [only] => format!("{only} is"),
        [a, b] => format!("{a} and {b} are"),
        many => {
            let (last, rest) = many.split_last().unwrap();
            format!("{}, and {last} are", rest.join(", "))
        }
    };
    format!(
        "Cannot recover missile parameters: {subject} missing. \
Time, position, velocity, acceleration, orientation, and mass are required. \
Nothing was guessed."
    )
}

pub struct RecoveryInput<'a> {
    pub time_s: &'a [f64],
    /// Longitude degrees, latitude degrees, altitude metres. Length is 3 × samples.
    pub lla: &'a [f32],
    pub velocity_ecef_mps: Option<&'a [f32]>,
    pub acceleration_ecef_mps2: Option<&'a [f32]>,
    /// Roll, pitch, yaw radians, 3 × samples. Pitch is nose elevation, yaw is heading from north toward east.
    pub orientation_rad: Option<&'a [f32]>,
    pub mass_kg: Option<&'a [f32]>,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
}

fn full(channel: Option<&[f32]>, samples: usize, width: usize) -> bool {
    channel.is_some_and(|values| values.len() == samples * width)
}

/// Build the fitter's state table, or the clear missing-channel error.
pub fn recovery_table(input: &RecoveryInput) -> Result<Value, String> {
    let samples = input.lla.len() / 3;
    if samples < 4 || input.lla.len() != samples * 3 {
        return Err("The trajectory needs at least four state samples to recover parameters.".into());
    }
    let mut missing = Vec::new();
    if input.time_s.len() != samples {
        missing.push("time");
    }
    if !full(input.velocity_ecef_mps, samples, 3) {
        missing.push("velocity");
    }
    if !full(input.acceleration_ecef_mps2, samples, 3) {
        missing.push("acceleration");
    }
    if !full(input.orientation_rad, samples, 3) {
        missing.push("orientation");
    }
    if !full(input.mass_kg, samples, 1) {
        missing.push("mass");
    }
    if !missing.is_empty() {
        return Err(missing_parameter_message(&missing));
    }
    let velocity = input.velocity_ecef_mps.unwrap();
    let acceleration = input.acceleration_ecef_mps2.unwrap();
    let orientation = input.orientation_rad.unwrap();
    let mass = input.mass_kg.unwrap();

    let lat0 = input.latitude_deg;
    let lon0 = input.longitude_deg;
    let alt0 = input.lla[2] as f64;
    let (x0, y0, z0) = lla_to_ecef(lat0, lon0, alt0);

    let mut time_s = Vec::with_capacity(samples);
    let mut east_m = Vec::with_capacity(samples);
    let mut north_m = Vec::with_capacity(samples);
    let mut up_m = Vec::with_capacity(samples);
    let mut vel_e = Vec::with_capacity(samples);
    let mut vel_n = Vec::with_capacity(samples);
    let mut vel_u = Vec::with_capacity(samples);
    let mut acc_e = Vec::with_capacity(samples);
    let mut acc_n = Vec::with_capacity(samples);
    let mut acc_u = Vec::with_capacity(samples);
    let mut yaw = Vec::with_capacity(samples);
    let mut pitch = Vec::with_capacity(samples);
    let mut roll = Vec::with_capacity(samples);
    let mut mass_kg = Vec::with_capacity(samples);

    for i in 0..samples {
        let lon = input.lla[i * 3] as f64;
        let lat = input.lla[i * 3 + 1] as f64;
        let alt = input.lla[i * 3 + 2] as f64;
        let (x, y, z) = lla_to_ecef(lat, lon, alt);
        let (east, north, _) = ecef_to_enu_vector(lat0, lon0, x - x0, y - y0, z - z0);
        let (ve, vn, vu) = ecef_to_enu_vector(
            lat0,
            lon0,
            velocity[i * 3] as f64,
            velocity[i * 3 + 1] as f64,
            velocity[i * 3 + 2] as f64,
        );
        let (ae, an, au) = ecef_to_enu_vector(
            lat0,
            lon0,
            acceleration[i * 3] as f64,
            acceleration[i * 3 + 1] as f64,
            acceleration[i * 3 + 2] as f64,
        );
        time_s.push(input.time_s[i]);
        east_m.push(east);
        north_m.push(north);
        up_m.push(alt);
        vel_e.push(ve);
        vel_n.push(vn);
        vel_u.push(vu);
        acc_e.push(ae);
        acc_n.push(an);
        acc_u.push(au);
        roll.push(orientation[i * 3] as f64);
        pitch.push(orientation[i * 3 + 1] as f64);
        yaw.push(orientation[i * 3 + 2] as f64);
        mass_kg.push(mass[i] as f64);
    }

    Ok(json!({
        "latitude_deg": lat0,
        "longitude_deg": lon0,
        "time_s": time_s,
        "east_m": east_m,
        "north_m": north_m,
        "up_m": up_m,
        "vel_east_mps": vel_e,
        "vel_north_mps": vel_n,
        "vel_up_mps": vel_u,
        "acc_east_mps2": acc_e,
        "acc_north_mps2": acc_n,
        "acc_up_mps2": acc_u,
        "yaw_rad": yaw,
        "pitch_rad": pitch,
        "roll_rad": roll,
        "mass_kg": mass_kg,
    }))
}

fn backend_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("rocketpy_backend")
        .join("recover.py")
}

fn python_bin() -> PathBuf {
    if let Ok(raw) = std::env::var("FAUXRRT_PYTHON") {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return path;
        }
    }
    let venv = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("rocketpy_backend")
        .join(".venv")
        .join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" });
    if venv.is_file() {
        return venv;
    }
    PathBuf::from(if cfg!(windows) { "python" } else { "python3" })
}

pub fn run_recovery(table: &Value) -> Result<Value, String> {
    let script = backend_script();
    if !script.is_file() {
        return Err(format!("recovery script was not found at {}", script.display()));
    }
    let payload = serde_json::to_vec(table).map_err(|err| format!("encode recovery states: {err}"))?;
    let mut command = Command::new(python_bin());
    command
        .arg("-u")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1");
    let mut child = command.spawn().map_err(|err| format!("start parameter recovery: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .map_err(|err| format!("write recovery states: {err}"))?;
    }
    let output = wait_child(child, Duration::from_secs(30))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or(stdout.trim());
    let parsed: Value = serde_json::from_str(line).map_err(|err| {
        let hint = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no Python error text");
        format!("parameter recovery did not return JSON ({err}). {hint}")
    })?;
    if parsed.get("ok").and_then(|value| value.as_bool()) == Some(false) {
        return Err(parsed
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("parameter recovery failed")
            .to_string());
    }
    if !output.status.success() {
        return Err(stderr.lines().last().unwrap_or("parameter recovery failed").to_string());
    }
    Ok(parsed)
}

fn wait_child(mut child: std::process::Child, limit: Duration) -> Result<std::process::Output, String> {
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().map_err(|err| format!("wait for recovery: {err}"));
            }
            Ok(None) if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("parameter recovery did not finish within 30s".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => return Err(format!("wait for recovery: {err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_acceleration_orientation_or_mass_is_the_clear_error() {
        let message = missing_parameter_message(&["acceleration"]);
        assert_eq!(
            message,
            "Cannot recover missile parameters: acceleration is missing. \
Time, position, velocity, acceleration, orientation, and mass are required. \
Nothing was guessed."
        );
        let both = missing_parameter_message(&["mass", "orientation"]);
        assert!(both.contains("orientation and mass are missing"));
        assert!(both.contains("Nothing was guessed"));

        let time = vec![0.0, 1.0, 2.0, 3.0];
        let lla = vec![0.0; 12];
        let vel = vec![0.0; 12];
        let input = RecoveryInput {
            time_s: &time,
            lla: &lla,
            velocity_ecef_mps: Some(&vel),
            acceleration_ecef_mps2: None,
            orientation_rad: None,
            mass_kg: None,
            latitude_deg: 32.0,
            longitude_deg: -106.0,
        };
        let err = recovery_table(&input).unwrap_err();
        assert!(err.contains("acceleration, orientation, and mass are missing"));
        assert!(err.contains("Nothing was guessed"));
        assert!(!err.to_lowercase().contains("cd"));
    }
}
