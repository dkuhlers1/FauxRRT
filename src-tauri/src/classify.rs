//! Local schema classification for trajectory text files.
//!
//! The model is Meta Llama 3.1 8B Instruct, quantized GGUF, run with llama.cpp.
//! Weights are downloaded on first use and are not part of the repository.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema::Delimiter;

pub const LOW_CONFIDENCE: f64 = 0.6;
pub const EXCERPT_LINES: usize = 40;

pub const GGUF_FILENAME: &str = "Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf";
pub const GGUF_URL: &str = "https://huggingface.co/bartowski/Meta-Llama-3.1-8B-Instruct-GGUF/resolve/main/Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf";
const GGUF_MIN_BYTES: u64 = 4_500_000_000;

const LLAMA_TAG: &str = "b11538";
const LLAMA_LINUX_URL: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b11538/llama-b11538-bin-ubuntu-x64.tar.gz";
const LLAMA_WINDOWS_URL: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b11538/llama-b11538-bin-win-cpu-x64.zip";

pub const SYSTEM_PROMPT: &str = r#"You classify trajectory state text files. Return one JSON object and nothing else. No markdown and no conversation.

Supported coordinate systems, and only these: NED, NEU, LLA, ECEF, ECI.
Supported units, and only these:
- position: m, km, ft
- velocity: m/s, ft/s, km/s
- acceleration: m/s^2, ft/s^2
- orientation: rad, deg
- mass: kg, lbm

Column roles, and only these names: time, pos_x, pos_y, pos_z, pos_lat, pos_lon, pos_alt, pos_n, pos_e, pos_d, pos_u, vel_x, vel_y, vel_z, vel_n, vel_e, vel_d, vel_u, acc_x, acc_y, acc_z, acc_n, acc_e, acc_d, acc_u, orientation_roll, orientation_pitch, orientation_yaw, mass, ignore.

Position columns are required.
LLA uses pos_lat, pos_lon, and pos_alt. The position unit is the altitude unit. Latitude and longitude stay in degrees.
ECEF and ECI use pos_x, pos_y, pos_z.
NED uses pos_n, pos_e, pos_d for north, east, and down.
NEU uses pos_n, pos_e, pos_u for north, east, and up.
Velocity, acceleration, and orientation use the same axis names when those columns exist.
Always include position, velocity, acceleration, and orientation under units. Include mass under units only when a column role is mass.

delimiter is one of "," , "\t" , ";" , "|" , or "whitespace".
header_lines is how many leading lines to skip before the first data row.
Set unsupported_flag to true when the coordinate system or any unit is outside the supported list. Do not rename an unsupported frame or unit into a supported one.
Set confidence_score below 0.6 when the layout is ambiguous. Do not guess past a low-confidence result.
reasoning is one short classification note, not a conversation.

Return exactly this JSON shape and no other field names:
{"header_lines":0,"delimiter":",","coordinate_system":"ECEF","units":{"position":"m","velocity":"m/s","acceleration":"m/s^2","orientation":"rad"},"columns":{"col_0":"time","col_1":"pos_x","col_2":"pos_y","col_3":"pos_z"},"confidence_score":0.85,"unsupported_flag":false,"reasoning":"one short note"}
"#;

const JSON_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["header_lines", "delimiter", "coordinate_system", "units", "columns", "confidence_score", "unsupported_flag", "reasoning"],
  "properties": {
    "header_lines": { "type": "integer", "minimum": 0 },
    "delimiter": { "type": "string" },
    "coordinate_system": { "type": "string" },
    "units": {
      "type": "object",
      "additionalProperties": false,
      "required": ["position"],
      "properties": {
        "position": { "type": "string" },
        "velocity": { "type": "string" },
        "acceleration": { "type": "string" },
        "orientation": { "type": "string" },
        "mass": { "type": "string" }
      }
    },
    "columns": { "type": "object", "additionalProperties": { "type": "string" } },
    "confidence_score": { "type": "number", "minimum": 0, "maximum": 1 },
    "unsupported_flag": { "type": "boolean" },
    "reasoning": { "type": "string" }
  }
}"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryClassification {
    pub header_lines: u32,
    pub delimiter: String,
    pub coordinate_system: String,
    pub units: ClassificationUnits,
    pub columns: BTreeMap<String, String>,
    pub confidence_score: f64,
    pub unsupported_flag: bool,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClassificationUnits {
    pub position: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceleration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mass: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaReviewStatus {
    Pending,
    Accepted,
    Edited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupedFile {
    pub path: String,
    pub name: String,
    pub excerpt: String,
    pub origin_lat: Option<f64>,
    pub origin_lon: Option<f64>,
    pub origin_alt_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaGroup {
    pub layout_id: String,
    pub classification: TrajectoryClassification,
    pub needs_manual: bool,
    pub manual_reason: String,
    pub files: Vec<GroupedFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchemaAssignment {
    pub paths: Vec<String>,
    pub classification: TrajectoryClassification,
    pub origin_lat: Option<f64>,
    pub origin_lon: Option<f64>,
    pub origin_alt_m: Option<f64>,
    pub user_assigned: bool,
}

pub trait TextCompleter {
    fn complete(&self, system: &str, user: &str) -> Result<String, String>;
}

pub fn excerpt_of(text: &str) -> String {
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i >= EXCERPT_LINES {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub fn classify_with(completer: &dyn TextCompleter, excerpt: &str) -> Result<TrajectoryClassification, String> {
    let first = completer.complete(SYSTEM_PROMPT, &user_message(excerpt))?;
    match accept_output(&first) {
        Ok(class) => Ok(class),
        Err(first_err) => {
            let second = completer.complete(SYSTEM_PROMPT, &retry_message(excerpt))?;
            accept_output(&second).map_err(|retry_err| {
                format!("classification was not the JSON object ({first_err}; retry: {retry_err})")
            })
        }
    }
}

fn user_message(excerpt: &str) -> String {
    format!("Classify this trajectory file excerpt.\n\n{excerpt}")
}

fn retry_message(excerpt: &str) -> String {
    format!(
        "The previous output was not the required JSON object. Return only that JSON object for this excerpt.\n\n{excerpt}"
    )
}

pub fn accept_output(raw: &str) -> Result<TrajectoryClassification, String> {
    let json = sole_json_object(raw)?;
    let mut class: TrajectoryClassification =
        serde_json::from_str(json).map_err(|err| format!("JSON did not match the classification object: {err}"))?;
    normalize_classification(&mut class);
    if !class.confidence_score.is_finite() || !(0.0..=1.0).contains(&class.confidence_score) {
        return Err("confidence_score must be between 0 and 1".into());
    }
    Ok(class)
}

fn sole_json_object(raw: &str) -> Result<&str, String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') {
        return Err("model output was not a JSON object".into());
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|_| "model output was not a single JSON object".to_string())?;
    if !value.is_object() {
        return Err("model output was not a JSON object".into());
    }
    Ok(trimmed)
}

pub fn normalize_classification(class: &mut TrajectoryClassification) {
    if let Some(frame) = canonical_frame(&class.coordinate_system) {
        class.coordinate_system = frame.to_string();
    }
    class.delimiter = canonical_delimiter(&class.delimiter);
    class.units.position = canonical_or_original(UnitKind::Position, &class.units.position);
    class.units.velocity = class.units.velocity.as_ref().map(|u| canonical_or_original(UnitKind::Velocity, u));
    class.units.acceleration = class
        .units
        .acceleration
        .as_ref()
        .map(|u| canonical_or_original(UnitKind::Acceleration, u));
    class.units.orientation = class
        .units
        .orientation
        .as_ref()
        .map(|u| canonical_or_original(UnitKind::Orientation, u));
    if class_has_role(class, "mass") {
        class.units.mass = class.units.mass.as_ref().map(|u| canonical_or_original(UnitKind::Mass, u));
    } else {
        class.units.mass = None;
    }
    let collapsed = class.reasoning.split_whitespace().collect::<Vec<_>>().join(" ");
    class.reasoning = if collapsed.chars().count() > 280 {
        let cut: String = collapsed.chars().take(279).collect();
        format!("{cut}…")
    } else {
        collapsed
    };
    if support_problem(class).is_some() {
        class.unsupported_flag = true;
    }
}

fn canonical_or_original(kind: UnitKind, raw: &str) -> String {
    canonical_unit(kind, raw).unwrap_or_else(|_| raw.trim().to_string())
}

pub fn canonical_frame(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "NED" | "NORTH EAST DOWN" | "NORTHEASTDOWN" => Some("NED"),
        "NEU" | "ENU" | "NORTH EAST UP" | "NORTHEASTUP" => Some("NEU"),
        "LLA" | "LAT LON ALT" | "LAT/LON/ALT" | "LATLONALT" => Some("LLA"),
        "ECEF" => Some("ECEF"),
        "ECI" => Some("ECI"),
        _ => None,
    }
}

pub fn canonical_delimiter(raw: &str) -> String {
    let t = raw.trim().to_ascii_lowercase();
    match t.as_str() {
        "," | "comma" | "csv" => ",".into(),
        "\t" | "\\t" | "tab" | "tsv" => "\t".into(),
        ";" | "semicolon" => ";".into(),
        "|" | "pipe" => "|".into(),
        "whitespace" | "space" | "spaces" | " " => "whitespace".into(),
        _ => raw.trim().to_string(),
    }
}

pub fn delimiter_of(raw: &str) -> Result<Delimiter, String> {
    match canonical_delimiter(raw).as_str() {
        "," => Ok(Delimiter::Comma),
        "\t" => Ok(Delimiter::Tab),
        ";" => Ok(Delimiter::Semicolon),
        "|" => Ok(Delimiter::Pipe),
        "whitespace" => Ok(Delimiter::Whitespace),
        other => Err(format!("delimiter {other:?} is not supported")),
    }
}

#[derive(Clone, Copy)]
enum UnitKind {
    Position,
    Velocity,
    Acceleration,
    Orientation,
    Mass,
}

pub fn canonical_unit(kind: UnitKind, raw: &str) -> Result<String, ()> {
    let t = raw.trim().to_ascii_lowercase().replace(' ', "");
    let t = t.replace("per", "/");
    let ok = match kind {
        UnitKind::Position => matches!(t.as_str(), "m" | "meter" | "meters" | "metre" | "metres" | "km" | "kilometer" | "kilometers" | "kilometre" | "kilometres" | "ft" | "foot" | "feet"),
        UnitKind::Velocity => matches!(t.as_str(), "m/s" | "mps" | "meter/s" | "meters/s" | "ft/s" | "fps" | "ft/sec" | "feet/s" | "km/s" | "kps"),
        UnitKind::Acceleration => matches!(t.as_str(), "m/s^2" | "m/s2" | "m/s²" | "ft/s^2" | "ft/s2" | "ft/s²" | "ft/sec^2"),
        UnitKind::Orientation => matches!(t.as_str(), "rad" | "radian" | "radians" | "deg" | "degree" | "degrees"),
        UnitKind::Mass => matches!(t.as_str(), "kg" | "kilogram" | "kilograms" | "lbm" | "lb" | "lbs" | "pound" | "pounds"),
    };
    if !ok {
        return Err(());
    }
    Ok(match kind {
        UnitKind::Position => match t.as_str() {
            "km" | "kilometer" | "kilometers" | "kilometre" | "kilometres" => "km",
            "ft" | "foot" | "feet" => "ft",
            _ => "m",
        },
        UnitKind::Velocity => match t.as_str() {
            "ft/s" | "fps" | "ft/sec" | "feet/s" => "ft/s",
            "km/s" | "kps" => "km/s",
            _ => "m/s",
        },
        UnitKind::Acceleration => match t.as_str() {
            "ft/s^2" | "ft/s2" | "ft/s²" | "ft/sec^2" => "ft/s^2",
            _ => "m/s^2",
        },
        UnitKind::Orientation => match t.as_str() {
            "deg" | "degree" | "degrees" => "deg",
            _ => "rad",
        },
        UnitKind::Mass => match t.as_str() {
            "lbm" | "lb" | "lbs" | "pound" | "pounds" => "lbm",
            _ => "kg",
        },
    }
    .to_string())
}

pub fn scale_length(unit: &str) -> Result<f64, String> {
    match canonical_unit(UnitKind::Position, unit).as_deref() {
        Ok("m") => Ok(1.0),
        Ok("km") => Ok(1000.0),
        Ok("ft") => Ok(0.3048),
        _ => Err(format!("position unit {unit} is not supported")),
    }
}

pub fn scale_speed(unit: &str) -> Result<f64, String> {
    match canonical_unit(UnitKind::Velocity, unit).as_deref() {
        Ok("m/s") => Ok(1.0),
        Ok("km/s") => Ok(1000.0),
        Ok("ft/s") => Ok(0.3048),
        _ => Err(format!("velocity unit {unit} is not supported")),
    }
}

pub fn scale_accel(unit: &str) -> Result<f64, String> {
    match canonical_unit(UnitKind::Acceleration, unit).as_deref() {
        Ok("m/s^2") => Ok(1.0),
        Ok("ft/s^2") => Ok(0.3048),
        _ => Err(format!("acceleration unit {unit} is not supported")),
    }
}

pub fn scale_angle_to_rad(unit: &str) -> Result<f64, String> {
    match canonical_unit(UnitKind::Orientation, unit).as_deref() {
        Ok("rad") => Ok(1.0),
        Ok("deg") => Ok(std::f64::consts::PI / 180.0),
        _ => Err(format!("orientation unit {unit} is not supported")),
    }
}

pub fn scale_mass(unit: &str) -> Result<f64, String> {
    match canonical_unit(UnitKind::Mass, unit).as_deref() {
        Ok("kg") => Ok(1.0),
        Ok("lbm") => Ok(0.45359237),
        _ => Err(format!("mass unit {unit} is not supported")),
    }
}

pub fn column_roles(class: &TrajectoryClassification) -> BTreeMap<usize, String> {
    let mut out = BTreeMap::new();
    for (key, role) in &class.columns {
        let Some(index) = key.strip_prefix("col_").and_then(|s| s.parse().ok()) else {
            continue;
        };
        let role = role.trim().to_ascii_lowercase();
        if role.is_empty() || role == "ignore" {
            continue;
        }
        out.insert(index, role);
    }
    out
}

fn class_has_role(class: &TrajectoryClassification, role: &str) -> bool {
    column_roles(class).values().any(|r| r == role)
}

fn role_index(roles: &BTreeMap<usize, String>, names: &[&str]) -> Option<usize> {
    roles.iter().find(|(_, role)| names.contains(&role.as_str())).map(|(i, _)| *i)
}

pub fn has_position_columns(class: &TrajectoryClassification) -> bool {
    let Some(frame) = canonical_frame(&class.coordinate_system) else {
        return false;
    };
    let roles = column_roles(class);
    match frame {
        "LLA" => role_index(&roles, &["pos_lat", "lat"]).is_some() && role_index(&roles, &["pos_lon", "lon"]).is_some(),
        "ECEF" | "ECI" => {
            role_index(&roles, &["pos_x"]).is_some()
                && role_index(&roles, &["pos_y"]).is_some()
                && role_index(&roles, &["pos_z"]).is_some()
        }
        "NED" => {
            role_index(&roles, &["pos_n", "pos_x"]).is_some()
                && role_index(&roles, &["pos_e", "pos_y"]).is_some()
                && role_index(&roles, &["pos_d", "pos_z"]).is_some()
        }
        "NEU" => {
            role_index(&roles, &["pos_n", "pos_x"]).is_some()
                && role_index(&roles, &["pos_e", "pos_y"]).is_some()
                && role_index(&roles, &["pos_u", "pos_z"]).is_some()
        }
        _ => false,
    }
}

fn channel_problem(class: &TrajectoryClassification, prefix: &str, unit: &Option<String>, kind: UnitKind) -> Option<String> {
    let roles = column_roles(class);
    let present = roles.values().any(|role| role.starts_with(prefix));
    if !present {
        return None;
    }
    match unit {
        None => Some(format!("{prefix} columns have no unit")),
        Some(unit) if canonical_unit(kind, unit).is_err() => Some(format!("{prefix} unit {unit} is not supported")),
        _ => None,
    }
}

pub fn support_problem(class: &TrajectoryClassification) -> Option<String> {
    if canonical_frame(&class.coordinate_system).is_none() {
        return Some(format!("coordinate system {} is not supported", class.coordinate_system));
    }
    if canonical_unit(UnitKind::Position, &class.units.position).is_err() {
        return Some(format!("position unit {} is not supported", class.units.position));
    }
    if delimiter_of(&class.delimiter).is_err() {
        return Some(format!("delimiter {} is not supported", class.delimiter));
    }
    for (label, unit, kind) in [
        ("velocity", &class.units.velocity, UnitKind::Velocity),
        ("acceleration", &class.units.acceleration, UnitKind::Acceleration),
        ("orientation", &class.units.orientation, UnitKind::Orientation),
    ] {
        if let Some(unit) = unit {
            if canonical_unit(kind, unit).is_err() {
                return Some(format!("{label} unit {unit} is not supported"));
            }
        }
    }
    if let Some(problem) = channel_problem(class, "vel_", &class.units.velocity, UnitKind::Velocity) {
        return Some(problem);
    }
    if let Some(problem) = channel_problem(class, "acc_", &class.units.acceleration, UnitKind::Acceleration) {
        return Some(problem);
    }
    if let Some(problem) = channel_problem(class, "orientation_", &class.units.orientation, UnitKind::Orientation) {
        return Some(problem);
    }
    if class_has_role(class, "mass") {
        match &class.units.mass {
            None => return Some("mass column has no unit".into()),
            Some(unit) if canonical_unit(UnitKind::Mass, unit).is_err() => {
                return Some(format!("mass unit {unit} is not supported"));
            }
            _ => {}
        }
    }
    None
}

pub fn manual_reason(class: &TrajectoryClassification) -> String {
    let mut parts = Vec::new();
    if let Some(problem) = support_problem(class) {
        parts.push(problem);
    }
    if class.unsupported_flag && support_problem(class).is_none() {
        parts.push("the classification was marked unsupported".into());
    }
    if !class.confidence_score.is_finite() || class.confidence_score < LOW_CONFIDENCE {
        parts.push(format!("confidence {:.2} is low", class.confidence_score));
    }
    if canonical_frame(&class.coordinate_system).is_some() && !has_position_columns(class) {
        parts.push("position columns are missing".into());
    }
    parts.join("; ")
}

pub fn needs_manual_override(class: &TrajectoryClassification) -> bool {
    !manual_reason(class).is_empty()
}

/// A detected schema may load. A user-assigned schema ignores confidence and still
/// refuses an unsupported frame, unit, delimiter, or missing position.
pub fn may_load(class: &TrajectoryClassification, user_assigned: bool) -> bool {
    let mut class = class.clone();
    normalize_classification(&mut class);
    if user_assigned {
        support_problem(&class).is_none() && has_position_columns(&class)
    } else {
        !needs_manual_override(&class)
    }
}

pub fn layout_id(class: &TrajectoryClassification) -> String {
    let mut class = class.clone();
    normalize_classification(&mut class);
    let layout = serde_json::json!({
        "header_lines": class.header_lines,
        "delimiter": class.delimiter,
        "coordinate_system": class.coordinate_system,
        "units": class.units,
        "columns": class.columns,
    });
    layout.to_string()
}

pub fn group_files(files: Vec<(GroupedFile, TrajectoryClassification)>) -> Vec<SchemaGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut buckets: BTreeMap<String, SchemaGroup> = BTreeMap::new();
    for (file, mut class) in files {
        normalize_classification(&mut class);
        let id = layout_id(&class);
        if !buckets.contains_key(&id) {
            order.push(id.clone());
            buckets.insert(
                id.clone(),
                SchemaGroup {
                    layout_id: id.clone(),
                    classification: class.clone(),
                    needs_manual: false,
                    manual_reason: String::new(),
                    files: Vec::new(),
                },
            );
        }
        let group = buckets.get_mut(&id).unwrap();
        let local = matches!(class.coordinate_system.as_str(), "NED" | "NEU");
        if local && file.origin_lat.is_none() {
            group.needs_manual = true;
        }
        if needs_manual_override(&class) {
            group.needs_manual = true;
        }
        group.files.push(file);
    }
    let mut groups = Vec::new();
    for id in order {
        let mut group = buckets.remove(&id).unwrap();
        let mut reasons = Vec::new();
        let layout_reason = manual_reason(&group.classification);
        if !layout_reason.is_empty() {
            reasons.push(layout_reason);
        }
        if matches!(group.classification.coordinate_system.as_str(), "NED" | "NEU")
            && group.files.iter().any(|file| file.origin_lat.is_none())
        {
            reasons.push("a local frame needs an origin latitude and longitude".into());
        }
        group.manual_reason = reasons.join("; ");
        group.needs_manual = !group.manual_reason.is_empty();
        groups.push(group);
    }
    groups
}

pub fn next_pending(status: &[SchemaReviewStatus]) -> Option<usize> {
    status.iter().position(|item| *item == SchemaReviewStatus::Pending)
}

/// A header origin stays with that file. An origin typed in the editor fills files that have none.
pub fn file_origin(
    header: Option<(f64, f64, f64)>,
    assigned: Option<(f64, f64, f64)>,
) -> Option<(f64, f64, f64)> {
    header.or(assigned)
}

pub fn origin_from_text(text: &str, header_lines: u32) -> Option<(f64, f64, f64)> {
    let limit = header_lines.max(12) as usize;
    let header: String = text.lines().take(limit).collect::<Vec<_>>().join("\n");
    let lat = labeled_number(&header, &["latitude", "lat"])?;
    let lon = labeled_number(&header, &["longitude", "lon", "long"])?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    let alt = labeled_number(&header, &["elevation", "origin_alt", "alt"]).unwrap_or(0.0);
    Some((lat, lon, alt))
}

fn labeled_number(hay: &str, labels: &[&str]) -> Option<f64> {
    let lower = hay.to_ascii_lowercase();
    for label in labels {
        let mut start = 0;
        while let Some(rel) = lower[start..].find(label) {
            let idx = start + rel;
            let before_ok = idx == 0 || !lower.as_bytes()[idx - 1].is_ascii_alphanumeric();
            let after = idx + label.len();
            let after_ok = after >= lower.len() || !lower.as_bytes()[after].is_ascii_alphanumeric();
            if before_ok && after_ok {
                if let Some(number) = number_at(&hay[after..]) {
                    return Some(number);
                }
            }
            start = idx + label.len();
        }
    }
    None
}

fn number_at(text: &str) -> Option<f64> {
    let rest = text.trim_start_matches(|c: char| !c.is_ascii_digit() && c != '-' && c != '+' && c != '.');
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '+' || c == '.' || c == 'e' || c == 'E'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

pub fn position_indexes(class: &TrajectoryClassification) -> Result<(Option<usize>, usize, usize, usize), String> {
    let frame = canonical_frame(&class.coordinate_system).ok_or_else(|| "unsupported coordinate system".to_string())?;
    let roles = column_roles(class);
    let time = role_index(&roles, &["time"]);
    let (a, b, c) = match frame {
        "LLA" => (
            role_index(&roles, &["pos_lat", "lat"]).ok_or("LLA needs pos_lat")?,
            role_index(&roles, &["pos_lon", "lon"]).ok_or("LLA needs pos_lon")?,
            role_index(&roles, &["pos_alt", "alt"]),
        ),
        "ECEF" | "ECI" => (
            role_index(&roles, &["pos_x"]).ok_or("missing pos_x")?,
            role_index(&roles, &["pos_y"]).ok_or("missing pos_y")?,
            Some(role_index(&roles, &["pos_z"]).ok_or("missing pos_z")?),
        ),
        "NED" => (
            role_index(&roles, &["pos_n", "pos_x"]).ok_or("NED needs pos_n")?,
            role_index(&roles, &["pos_e", "pos_y"]).ok_or("NED needs pos_e")?,
            Some(role_index(&roles, &["pos_d", "pos_z"]).ok_or("NED needs pos_d")?),
        ),
        "NEU" => (
            role_index(&roles, &["pos_n", "pos_x"]).ok_or("NEU needs pos_n")?,
            role_index(&roles, &["pos_e", "pos_y"]).ok_or("NEU needs pos_e")?,
            Some(role_index(&roles, &["pos_u", "pos_z"]).ok_or("NEU needs pos_u")?),
        ),
        _ => return Err("unsupported coordinate system".into()),
    };
    if frame == "LLA" {
        Ok((time, a, b, c.unwrap_or(usize::MAX)))
    } else {
        Ok((time, a, b, c.unwrap()))
    }
}

pub struct LlamaRuntime {
    pub cli: PathBuf,
    pub model: PathBuf,
}

impl LlamaRuntime {
    pub fn ensure(data_dir: &Path) -> Result<Self, String> {
        let model = ensure_model(data_dir)?;
        let cli = ensure_cli(data_dir)?;
        Ok(Self { cli, model })
    }
}

impl TextCompleter for LlamaRuntime {
    fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        let dir = std::env::temp_dir().join(format!("fauxrrt-llama-{}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let prompt_path = dir.join("prompt.txt");
        let schema_path = dir.join("schema.json");
        fs::write(&prompt_path, llama_prompt(system, user)).map_err(|e| e.to_string())?;
        fs::write(&schema_path, JSON_SCHEMA).map_err(|e| e.to_string())?;
        let help = cli_help(&self.cli);
        let mut cmd = Command::new(&self.cli);
        cmd.arg("-m").arg(&self.model);
        cmd.arg("-n").arg("700");
        if help_has(&help, "--temp") {
            cmd.arg("--temp").arg("0");
        } else if help_has(&help, "--temperature") {
            cmd.arg("--temperature").arg("0");
        }
        if help_has(&help, "--no-conversation") {
            cmd.arg("--no-conversation");
        } else if help_has(&help, "-no-cnv") {
            cmd.arg("-no-cnv");
        }
        if help_has(&help, "--no-display-prompt") {
            cmd.arg("--no-display-prompt");
        }
        if help_has(&help, "--json-schema-file") {
            cmd.arg("--json-schema-file").arg(&schema_path);
        } else if help_has(&help, "--grammar-file") {
            let grammar_path = dir.join("schema.gbnf");
            fs::write(&grammar_path, classification_grammar()).map_err(|e| e.to_string())?;
            cmd.arg("--grammar-file").arg(&grammar_path);
        }
        if help_has(&help, "-f") || help_has(&help, "--file") {
            cmd.arg("-f").arg(&prompt_path);
        } else {
            let prompt = fs::read_to_string(&prompt_path).unwrap_or_default();
            cmd.arg("-p").arg(prompt);
        }
        let output = cmd.output().map_err(|e| format!("could not run {}: {e}", self.cli.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if stdout.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no llama.cpp output");
            return Err(format!("llama.cpp failed: {tail}"));
        }
        Ok(stdout)
    }
}

fn llama_prompt(system: &str, user: &str) -> String {
    format!(
        "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n{system}<|eot_id|><|start_header_id|>user<|end_header_id|>\n\n{user}<|eot_id|><|start_header_id|>assistant<|end_header_id|>\n\n"
    )
}

fn help_has(help: &str, flag: &str) -> bool {
    help.split_whitespace().any(|word| {
        word == flag || word.strip_suffix(',').is_some_and(|w| w == flag) || word.starts_with(&format!("{flag}="))
    })
}

fn cli_help(cli: &Path) -> String {
    let output = Command::new(cli).arg("--help").output();
    match output {
        Ok(out) => format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(_) => String::new(),
    }
}

fn classification_grammar() -> &'static str {
    r#"root ::= "{" ws "\"header_lines\":" ws number ws "," ws "\"delimiter\":" ws string ws "," ws "\"coordinate_system\":" ws string ws "," ws "\"units\":" ws units ws "," ws "\"columns\":" ws columns ws "," ws "\"confidence_score\":" ws number ws "," ws "\"unsupported_flag\":" ws boolean ws "," ws "\"reasoning\":" ws string ws "}"
ws ::= [ \t\n]*
number ::= "-"? [0-9]+ ("." [0-9]+)?
boolean ::= "true" | "false"
string ::= "\"" ([^"\\] | "\\" .)* "\""
units ::= "{" ws "\"position\":" ws string (ws "," ws "\"velocity\":" ws string)? (ws "," ws "\"acceleration\":" ws string)? (ws "," ws "\"orientation\":" ws string)? (ws "," ws "\"mass\":" ws string)? ws "}"
columns ::= "{" ws pair (ws "," ws pair)* ws "}"
pair ::= string ws ":" ws string
"#
}

pub fn ensure_model(data_dir: &Path) -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("FAUXRRT_LLAMA_MODEL") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("FAUXRRT_LLAMA_MODEL is not a file: {}", path.display()));
    }
    let dest = data_dir.join("models").join(GGUF_FILENAME);
    if dest.is_file() && fs::metadata(&dest).map(|m| m.len() >= GGUF_MIN_BYTES).unwrap_or(false) {
        return Ok(dest);
    }
    fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    download_to(GGUF_URL, &dest).map_err(|err| {
        format!(
            "{err}. Llama-3.1-8B-Instruct Q4_K_M can also be placed at {}. Source: {GGUF_URL}",
            dest.display()
        )
    })?;
    Ok(dest)
}

pub fn ensure_cli(data_dir: &Path) -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("FAUXRRT_LLAMA_CLI") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("FAUXRRT_LLAMA_CLI is not a file: {}", path.display()));
    }
    if let Some(found) = which_on_path(if cfg!(windows) { "llama-cli.exe" } else { "llama-cli" }) {
        return Ok(found);
    }
    let root = data_dir.join("llama.cpp");
    if let Some(found) = find_cli(&root) {
        return Ok(found);
    }
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let (name, url) = if cfg!(windows) {
        (format!("llama-{LLAMA_TAG}-bin-win-cpu-x64.zip"), LLAMA_WINDOWS_URL)
    } else {
        (format!("llama-{LLAMA_TAG}-bin-ubuntu-x64.tar.gz"), LLAMA_LINUX_URL)
    };
    let archive = root.join(name);
    download_to(url, &archive)?;
    extract_archive(&archive, &root)?;
    find_cli(&root).ok_or_else(|| format!("llama.cpp archive did not contain llama-cli ({url})"))
}

fn find_cli(root: &Path) -> Option<PathBuf> {
    let names = ["llama-cli", "llama-cli.exe"];
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .find(|entry| names.iter().any(|name| entry.file_name().to_string_lossy().eq_ignore_ascii_case(name)))
        .map(|entry| entry.into_path())
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    if dest.is_file() && fs::metadata(dest).map(|m| m.len() > 0).unwrap_or(false) {
        if !dest.extension().is_some_and(|ext| ext == "gguf") || fs::metadata(dest).map(|m| m.len() >= GGUF_MIN_BYTES).unwrap_or(false)
        {
            return Ok(());
        }
    }
    let tmp = dest.with_extension("partial");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout(Duration::from_secs(60 * 180))
        .build();
    let response = agent.get(url).call().map_err(|err| format!("download {url} failed: {err}"))?;
    let mut reader = response.into_reader();
    let mut file = fs::File::create(&tmp).map_err(|err| format!("create {}: {err}", tmp.display()))?;
    let mut buf = [0u8; 1024 * 256];
    loop {
        let n = reader.read(&mut buf).map_err(|err| format!("read {url}: {err}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    }
    file.sync_all().ok();
    fs::rename(&tmp, dest).map_err(|err| format!("rename download: {err}"))?;
    Ok(())
}

fn extract_archive(archive: &Path, dest: &Path) -> Result<(), String> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let status = if name.ends_with(".zip") {
        if cfg!(windows) {
            Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "Expand-Archive -Force -LiteralPath '{}' -DestinationPath '{}'",
                        archive.display(),
                        dest.display()
                    ),
                ])
                .status()
        } else {
            Command::new("unzip").args(["-o", &archive.to_string_lossy(), "-d", &dest.to_string_lossy()]).status()
        }
    } else {
        Command::new("tar")
            .args(["-xzf", &archive.to_string_lossy(), "-C", &dest.to_string_lossy()])
            .status()
    }
    .map_err(|err| format!("extract {}: {err}", archive.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("extract {} failed", archive.display()))
    }
}

pub fn classify_text(text: &str, data_dir: &Path) -> Result<TrajectoryClassification, String> {
    let runtime = LlamaRuntime::ensure(data_dir)?;
    classify_with(&runtime, &excerpt_of(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn sample_ecef() -> TrajectoryClassification {
        let mut columns = BTreeMap::new();
        columns.insert("col_0".into(), "time".into());
        columns.insert("col_1".into(), "pos_x".into());
        columns.insert("col_2".into(), "pos_y".into());
        columns.insert("col_3".into(), "pos_z".into());
        TrajectoryClassification {
            header_lines: 1,
            delimiter: ",".into(),
            coordinate_system: "ECEF".into(),
            units: ClassificationUnits {
                position: "m".into(),
                velocity: Some("m/s".into()),
                acceleration: Some("m/s^2".into()),
                orientation: Some("rad".into()),
                mass: None,
            },
            columns,
            confidence_score: 0.9,
            unsupported_flag: false,
            reasoning: "ECEF metres.".into(),
        }
    }

    struct Scripted {
        replies: Mutex<Vec<String>>,
        calls: Mutex<usize>,
    }

    impl TextCompleter for Scripted {
        fn complete(&self, _system: &str, _user: &str) -> Result<String, String> {
            *self.calls.lock().unwrap() += 1;
            let mut replies = self.replies.lock().unwrap();
            if replies.is_empty() {
                return Err("no scripted reply".into());
            }
            Ok(replies.remove(0))
        }
    }

    #[test]
    fn prompt_lists_supported_frames_units_and_exact_keys() {
        for frame in ["NED", "NEU", "LLA", "ECEF", "ECI"] {
            assert!(SYSTEM_PROMPT.contains(frame), "{frame}");
        }
        for unit in ["m", "ft", "m/s", "ft/s", "m/s^2", "ft/s^2", "rad", "deg", "kg", "lbm", "km"] {
            assert!(SYSTEM_PROMPT.contains(unit), "{unit}");
        }
        for key in [
            "header_lines",
            "delimiter",
            "coordinate_system",
            "confidence_score",
            "unsupported_flag",
            "reasoning",
            "pos_x",
            "vel_",
            "orientation_",
            "mass",
        ] {
            assert!(SYSTEM_PROMPT.contains(key), "{key}");
        }
        assert!(!SYSTEM_PROMPT.contains("header_line_count"));
        assert!(!SYSTEM_PROMPT.contains("needs_manual_override"));
        assert!(!SYSTEM_PROMPT.contains("coordinate_system_supported"));
        assert!(GGUF_URL.contains("Llama-3.1-8B-Instruct"));
        assert!(GGUF_FILENAME.contains("Q4_K_M"));
        assert!(!GGUF_URL.to_ascii_lowercase().contains("mistral"));
        assert!(!GGUF_URL.to_ascii_lowercase().contains("nemo"));
    }

    #[test]
    fn rejects_conversation_and_retries_once() {
        let good = serde_json::to_string(&sample_ecef()).unwrap();
        let scripted = Scripted {
            replies: Mutex::new(vec!["Sure, this file looks like ECEF.".into(), good]),
            calls: Mutex::new(0),
        };
        let class = classify_with(&scripted, "t,x,y,z\n").unwrap();
        assert_eq!(class.coordinate_system, "ECEF");
        assert_eq!(*scripted.calls.lock().unwrap(), 2);
    }

    #[test]
    fn accepts_only_the_classification_object() {
        let raw = serde_json::to_string(&sample_ecef()).unwrap();
        let class = accept_output(&raw).unwrap();
        assert!(!class.unsupported_flag);
        let extra = r#"{"header_lines":1,"delimiter":",","coordinate_system":"ECEF","units":{"position":"m"},"columns":{"col_0":"pos_x","col_1":"pos_y","col_2":"pos_z"},"confidence_score":0.9,"unsupported_flag":false,"reasoning":"ok","warnings":[]}"#;
        assert!(accept_output(extra).is_err());
        assert!(accept_output("```json\n{\"header_lines\":0}\n```").is_err());
    }

    #[test]
    fn unsupported_frame_and_low_confidence_need_manual_assignment() {
        let mut class = sample_ecef();
        class.coordinate_system = "BODY".into();
        normalize_classification(&mut class);
        assert!(class.unsupported_flag);
        assert!(needs_manual_override(&class));
        assert!(!may_load(&class, false));

        let mut low = sample_ecef();
        low.confidence_score = 0.2;
        assert!(needs_manual_override(&low));
        assert!(!may_load(&low, false));
        assert!(may_load(&low, true));
        assert!(!may_load(&class, true));
    }

    #[test]
    fn mass_unit_is_kept_only_with_a_mass_column() {
        let mut class = sample_ecef();
        class.units.mass = Some("kg".into());
        normalize_classification(&mut class);
        assert!(class.units.mass.is_none());
        class.columns.insert("col_4".into(), "mass".into());
        class.units.mass = Some("pounds".into());
        normalize_classification(&mut class);
        assert_eq!(class.units.mass.as_deref(), Some("lbm"));
        assert!(!class.unsupported_flag);
    }

    #[test]
    fn identical_layouts_share_one_schema_group() {
        let mut a = sample_ecef();
        a.reasoning = "first file".into();
        a.confidence_score = 0.91;
        let mut b = sample_ecef();
        b.reasoning = "second file".into();
        b.confidence_score = 0.88;
        let mut other = sample_ecef();
        other.delimiter = "tab".into();
        let groups = group_files(vec![
            (file("a.csv"), a),
            (file("b.csv"), b),
            (file("c.csv"), other),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].files.len(), 2);
        assert!(!groups[0].needs_manual);
    }

    #[test]
    fn accepted_schemas_are_not_asked_again() {
        let mut status = [
            SchemaReviewStatus::Pending,
            SchemaReviewStatus::Pending,
            SchemaReviewStatus::Pending,
        ];
        assert_eq!(next_pending(&status), Some(0));
        status[0] = SchemaReviewStatus::Accepted;
        assert_eq!(next_pending(&status), Some(1));
        status[1] = SchemaReviewStatus::Edited;
        assert_eq!(next_pending(&status), Some(2));
        status[2] = SchemaReviewStatus::Accepted;
        assert_eq!(next_pending(&status), None);
    }

    #[test]
    fn model_unsupported_flag_is_kept_and_blocks_autoload() {
        let mut class = sample_ecef();
        class.unsupported_flag = true;
        normalize_classification(&mut class);
        assert!(class.unsupported_flag);
        assert!(needs_manual_override(&class));
        assert!(!may_load(&class, false));
        assert!(may_load(&class, true));
    }

    #[test]
    fn assigned_origin_fills_only_when_the_header_has_none() {
        let header = Some((32.0, -106.0, 10.0));
        let assigned = Some((1.0, 2.0, 3.0));
        assert_eq!(file_origin(header, assigned), header);
        assert_eq!(file_origin(None, assigned), assigned);
    }

    #[test]
    fn local_frame_without_an_origin_needs_manual() {
        let mut class = sample_ecef();
        class.coordinate_system = "NED".into();
        class.columns.insert("col_1".into(), "pos_n".into());
        class.columns.insert("col_2".into(), "pos_e".into());
        class.columns.insert("col_3".into(), "pos_d".into());
        let groups = group_files(vec![(file("ned.csv"), class)]);
        assert!(groups[0].needs_manual);
        assert!(groups[0].manual_reason.contains("origin"));
    }

    #[test]
    fn header_can_carry_a_local_origin() {
        let text = "origin latitude 32.4 longitude -106.4 elevation 1200\nn,e,d\n0,0,0\n";
        let (lat, lon, alt) = origin_from_text(text, 1).unwrap();
        assert!((lat - 32.4).abs() < 1e-9);
        assert!((lon + 106.4).abs() < 1e-9);
        assert!((alt - 1200.0).abs() < 1e-6);
    }

    fn file(name: &str) -> GroupedFile {
        GroupedFile {
            path: name.into(),
            name: name.into(),
            excerpt: "t,x,y,z\n".into(),
            origin_lat: None,
            origin_lon: None,
            origin_alt_m: None,
        }
    }

    #[test]
    fn live_llama_is_optional() {
        let model = std::env::var("FAUXRRT_LLAMA_MODEL").ok();
        let cli = std::env::var("FAUXRRT_LLAMA_CLI").ok();
        let (Some(model), Some(cli)) = (model, cli) else {
            return;
        };
        if !Path::new(&model).is_file() || !Path::new(&cli).is_file() {
            return;
        }
        let runtime = LlamaRuntime {
            cli: PathBuf::from(cli),
            model: PathBuf::from(model),
        };
        let excerpt = "time,lat,lon,alt\n0,32.4,-106.4,1000\n1,32.41,-106.39,1100\n";
        let class = classify_with(&runtime, excerpt).expect("live classification");
        assert_eq!(class.coordinate_system, "LLA");
        assert!(has_position_columns(&class));
    }
}
