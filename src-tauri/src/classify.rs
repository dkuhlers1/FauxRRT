//! Local schema classification for trajectory text files.
//!
//! The model is Meta Llama 3.1 8B Instruct, quantized GGUF, served by one resident
//! llama-server process. Weights are downloaded on first use and are not part of the repository.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema::{detect_preview, detect_schema, ColumnRole, Delimiter, DetectedSchema, Frame};

pub const LOW_CONFIDENCE: f64 = 0.6;
pub const EXCERPT_LINES: usize = 40;

pub const GGUF_FILENAME: &str = "Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf";
pub const GGUF_URL: &str = "https://huggingface.co/bartowski/Meta-Llama-3.1-8B-Instruct-GGUF/resolve/main/Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf";
const GGUF_MIN_BYTES: u64 = 4_500_000_000;

const LLAMA_TAG: &str = "b11538";
const LLAMA_RELEASE: &str = "https://github.com/ggml-org/llama.cpp/releases/download/b11538";
/// One classification call. Long enough for an 8B model on CPU, short enough that a hung llama-server ends.
pub const INFERENCE_TIMEOUT: Duration = Duration::from_secs(8 * 60);
/// Loading the GGUF into GPU memory. This is paid once per app session, not once per file.
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(180);
pub const CANCELLED: &str = "classification cancelled";

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

Read the names in the excerpt. They are not a fixed dictionary, and a named header is still your job.
lat, latitude, lon, longitude, alt, and altitude are LLA position: pos_lat, pos_lon, pos_alt.
x-ecr, y-ecr, and z-ecr are Earth-centered rotating position, which is ECEF: pos_x, pos_y, and pos_z. Match each column by the axis in its name. z-ecr may appear before y-ecr.
north, east, and down are NED: pos_n, pos_e, and pos_d.
Return that identification only. Do not convert coordinates into another frame.

frames.position, frames.velocity, and frames.acceleration are separate coordinate systems. They may differ. Position in ECEF with velocity in NED is valid. Omit frames.velocity when the file has no velocity columns. Omit frames.acceleration when the file has no acceleration columns. Do not use one coordinate system for the whole file.
Always include position under units. Include velocity, acceleration, orientation, and mass under units only when those columns exist.

delimiter is one of "," , "\t" , ";" , "|" , or "whitespace".
header_lines is how many leading lines to skip before the first data row.
Set unsupported_flag to true when a coordinate system or any unit is outside the supported list. Do not rename an unsupported frame or unit into a supported one.
Set confidence_score below 0.6 when the layout is ambiguous. Do not guess past a low-confidence result.
reasoning is one short classification note, not a conversation.

Return exactly this JSON shape and no other field names:
{"header_lines":1,"delimiter":",","frames":{"position":"ECEF","velocity":"NED"},"units":{"position":"m","velocity":"m/s"},"columns":{"col_0":"time","col_1":"pos_x","col_2":"pos_y","col_3":"pos_z","col_4":"vel_n","col_5":"vel_e","col_6":"vel_d"},"confidence_score":0.85,"unsupported_flag":false,"reasoning":"position ECEF, velocity NED"}
"#;

const JSON_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["header_lines", "delimiter", "frames", "units", "columns", "confidence_score", "unsupported_flag", "reasoning"],
  "properties": {
    "header_lines": { "type": "integer", "minimum": 0 },
    "delimiter": { "type": "string" },
    "frames": {
      "type": "object",
      "additionalProperties": false,
      "required": ["position"],
      "properties": {
        "position": { "type": "string" },
        "velocity": { "type": "string" },
        "acceleration": { "type": "string" }
      }
    },
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

/// Coordinate system of one state group. Position, velocity, and acceleration are independent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChannelFrames {
    pub position: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceleration: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryClassification {
    pub header_lines: u32,
    pub delimiter: String,
    pub frames: ChannelFrames,
    pub units: ClassificationUnits,
    pub columns: BTreeMap<String, String>,
    pub confidence_score: f64,
    pub unsupported_flag: bool,
    pub reasoning: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassificationWire {
    header_lines: u32,
    delimiter: String,
    /// Older files stored one frame for the whole file. The loader expands it per channel.
    #[serde(default)]
    coordinate_system: Option<String>,
    #[serde(default)]
    frames: Option<ChannelFrames>,
    units: ClassificationUnits,
    columns: BTreeMap<String, String>,
    confidence_score: f64,
    unsupported_flag: bool,
    reasoning: String,
}

impl<'de> Deserialize<'de> for TrajectoryClassification {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ClassificationWire::deserialize(deserializer)?;
        let frames = match wire.frames {
            Some(frames) => frames,
            None => {
                let frame = wire.coordinate_system.unwrap_or_default();
                let velocity = wire.columns.values().any(|role| role.starts_with("vel_")).then(|| frame.clone());
                let acceleration = wire
                    .columns
                    .values()
                    .any(|role| role.starts_with("acc_"))
                    .then(|| frame.clone());
                ChannelFrames { position: frame, velocity, acceleration }
            }
        };
        Ok(TrajectoryClassification {
            header_lines: wire.header_lines,
            delimiter: wire.delimiter,
            frames,
            units: wire.units,
            columns: wire.columns,
            confidence_score: wire.confidence_score,
            unsupported_flag: wire.unsupported_flag,
            reasoning: wire.reasoning,
        })
    }
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

#[cfg(test)]
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
    /// The model did not return JSON. The editor opens with this group's best guess.
    #[serde(default)]
    pub editor_required: bool,
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
    let objects = json_objects(raw);
    if objects.is_empty() {
        return Err("model output was not a JSON object".into());
    }
    // The last object is the completion. An echoed prompt can contain an earlier example.
    let mut last_err = "model output was not a JSON object".to_string();
    for object in objects.iter().rev() {
        match serde_json::from_str::<TrajectoryClassification>(object) {
            Ok(mut class) => {
                normalize_classification(&mut class);
                if !class.confidence_score.is_finite() || !(0.0..=1.0).contains(&class.confidence_score) {
                    last_err = "confidence_score must be between 0 and 1".into();
                    continue;
                }
                return Ok(class);
            }
            Err(err) => last_err = format!("JSON did not match the classification object: {err}"),
        }
    }
    Err(last_err)
}

/// Every balanced `{...}` value in `raw`, including ones that follow a log line.
fn json_objects(raw: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        let Some(rel) = raw[index..].find('{') else {
            break;
        };
        let start = index + rel;
        let slice = &raw[start..];
        match balanced_object_end(slice) {
            Some(end) => {
                let object = &slice[..=end];
                if serde_json::from_str::<Value>(object)
                    .ok()
                    .is_some_and(|value| value.is_object())
                {
                    out.push(object);
                }
                index = start + end + 1;
            }
            None => index = start + '{'.len_utf8(),
        }
    }
    out
}

/// llama.cpp on Windows can write the completion as UTF-16 when stdout is a pipe.
pub fn decode_process_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return String::from_utf16_lossy(&utf16_units(&bytes[2..], true));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return String::from_utf16_lossy(&utf16_units(&bytes[2..], false));
    }
    if looks_utf16_le(bytes) {
        return String::from_utf16_lossy(&utf16_units(bytes, true));
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn looks_utf16_le(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || bytes.len() % 2 != 0 {
        return false;
    }
    let highs = bytes.len() / 2;
    let zeros = bytes.iter().skip(1).step_by(2).filter(|byte| **byte == 0).count();
    zeros * 4 >= highs * 3
}

fn utf16_units(bytes: &[u8], little: bool) -> Vec<u16> {
    bytes
        .chunks(2)
        .map(|chunk| {
            let lo = chunk[0];
            let hi = chunk.get(1).copied().unwrap_or(0);
            if little {
                u16::from_le_bytes([lo, hi])
            } else {
                u16::from_be_bytes([lo, hi])
            }
        })
        .collect()
}

pub fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Load failures llama.cpp prints instead of a completion. They are not model text.
pub fn runtime_failure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "failed to allocate",
        "out of memory",
        "unable to load model",
        "not enough memory",
        "insufficient memory",
        "bad_alloc",
        "cudamalloc failed",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn balanced_object_end(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (index, ch) in text.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn free_vram_mib() -> Option<u64> {
    let mut cmd = Command::new("nvidia-smi");
    cmd.args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"]);
    let out = run_bounded(cmd, Duration::from_secs(3), &AtomicBool::new(false)).ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .min()
}

fn canon_owned(raw: &str) -> String {
    canonical_frame(raw).unwrap_or(raw).to_string()
}

pub fn normalize_classification(class: &mut TrajectoryClassification) {
    class.frames.position = canon_owned(&class.frames.position);
    if !class_has_prefix(class, "vel_") {
        class.frames.velocity = None;
        class.units.velocity = None;
    } else if let Some(frame) = class.frames.velocity.as_ref() {
        class.frames.velocity = Some(canon_owned(frame));
    }
    if !class_has_prefix(class, "acc_") {
        class.frames.acceleration = None;
        class.units.acceleration = None;
    } else if let Some(frame) = class.frames.acceleration.as_ref() {
        class.frames.acceleration = Some(canon_owned(frame));
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

fn canonical_unit(kind: UnitKind, raw: &str) -> Result<String, ()> {
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

fn class_has_prefix(class: &TrajectoryClassification, prefix: &str) -> bool {
    column_roles(class).values().any(|role| role.starts_with(prefix))
}

fn frame_problem(label: &str, raw: &str) -> Option<String> {
    if canonical_frame(raw).is_none() {
        Some(format!("{label} coordinate system {raw} is not supported"))
    } else {
        None
    }
}

fn role_index(roles: &BTreeMap<usize, String>, names: &[&str]) -> Option<usize> {
    roles.iter().find(|(_, role)| names.contains(&role.as_str())).map(|(i, _)| *i)
}

pub fn has_position_columns(class: &TrajectoryClassification) -> bool {
    let Some(frame) = canonical_frame(&class.frames.position) else {
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
    if let Some(problem) = frame_problem("position", &class.frames.position) {
        return Some(problem);
    }
    if class_has_prefix(class, "vel_") {
        match class.frames.velocity.as_deref() {
            None => return Some("velocity columns have no coordinate system".into()),
            Some(frame) => {
                if let Some(problem) = frame_problem("velocity", frame) {
                    return Some(problem);
                }
            }
        }
    }
    if class_has_prefix(class, "acc_") {
        match class.frames.acceleration.as_deref() {
            None => return Some("acceleration columns have no coordinate system".into()),
            Some(frame) => {
                if let Some(problem) = frame_problem("acceleration", frame) {
                    return Some(problem);
                }
            }
        }
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
    if canonical_frame(&class.frames.position).is_some() && !has_position_columns(class) {
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
        "frames": class.frames,
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
                    editor_required: false,
                },
            );
        }
        let group = buckets.get_mut(&id).unwrap();
        let local = matches!(canonical_frame(&class.frames.position), Some("NED" | "NEU"));
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
        if matches!(canonical_frame(&group.classification.frames.position), Some("NED" | "NEU"))
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

/// Low-confidence column guess for the manual editor when Llama does not return JSON.
/// Loading a file does not use this in place of the model.
pub fn heuristic_classification(text: &str) -> Result<TrajectoryClassification, String> {
    let preview = detect_preview(text).ok_or_else(|| "could not detect a columnar trajectory file format".to_string())?;
    Ok(classification_from_detected(&detect_schema(&preview)))
}

fn classification_from_detected(schema: &DetectedSchema) -> TrajectoryClassification {
    let mut columns = BTreeMap::new();
    for col in &schema.columns {
        let role = match col.role {
            ColumnRole::Time => "time",
            ColumnRole::Lat => "pos_lat",
            ColumnRole::Lon => "pos_lon",
            ColumnRole::Alt => "pos_alt",
            ColumnRole::X => "pos_x",
            ColumnRole::Y => "pos_y",
            ColumnRole::Z => "pos_z",
            ColumnRole::Other => continue,
        };
        columns.insert(format!("col_{}", col.index), role.to_string());
    }
    let km = schema.columns.iter().any(|col| {
        matches!(col.role, ColumnRole::Alt | ColumnRole::X | ColumnRole::Y | ColumnRole::Z)
            && col.name.to_ascii_lowercase().contains("km")
    });
    let mut class = TrajectoryClassification {
        header_lines: if schema.has_header { 1 } else { 0 },
        delimiter: schema.delimiter.clone(),
        frames: ChannelFrames {
            position: match schema.frame {
                Frame::Lla => "LLA",
                Frame::Ecef => "ECEF",
            }
            .to_string(),
            velocity: None,
            acceleration: None,
        },
        units: ClassificationUnits {
            position: if km { "km" } else { "m" }.to_string(),
            velocity: None,
            acceleration: None,
            orientation: None,
            mass: None,
        },
        columns,
        confidence_score: (schema.confidence as f64).clamp(0.0, 1.0),
        unsupported_flag: false,
        reasoning: "Detected from the column names because the Llama weight was not available.".into(),
    };
    normalize_classification(&mut class);
    class
}

/// Guess used only to fill the manual editor when Llama does not return JSON.
pub fn partial_schema_guess(text: &str) -> TrajectoryClassification {
    let mut class = heuristic_classification(text).unwrap_or_else(|_| TrajectoryClassification {
        header_lines: 0,
        delimiter: ",".into(),
        frames: ChannelFrames {
            position: "LLA".into(),
            velocity: None,
            acceleration: None,
        },
        units: ClassificationUnits {
            position: "m".into(),
            velocity: None,
            acceleration: None,
            orientation: None,
            mass: None,
        },
        columns: BTreeMap::new(),
        confidence_score: 0.0,
        unsupported_flag: false,
        reasoning: String::new(),
    });
    class.confidence_score = class.confidence_score.min(0.2);
    if class.reasoning.trim().is_empty() {
        class.reasoning = "Partial guess from the file text.".into();
    }
    normalize_classification(&mut class);
    class
}

#[cfg(test)]
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
    let frame = canonical_frame(&class.frames.position).ok_or_else(|| "unsupported coordinate system".to_string())?;
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

#[derive(Debug, Clone)]
pub struct ClassifyNote {
    pub label: String,
    pub bytes: u64,
    pub bytes_total: u64,
    pub device: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackend {
    Cuda,
    Vulkan,
    Cpu,
}

/// NVIDIA uses the CUDA llama.cpp build. Any other GPU uses Vulkan. No GPU uses CPU.
pub fn choose_backend(nvidia: bool, vulkan: bool) -> GpuBackend {
    if nvidia {
        GpuBackend::Cuda
    } else if vulkan {
        GpuBackend::Vulkan
    } else {
        GpuBackend::Cpu
    }
}

pub fn device_name(backend: GpuBackend) -> &'static str {
    match backend {
        GpuBackend::Cuda => "CUDA",
        GpuBackend::Vulkan => "Vulkan",
        GpuBackend::Cpu => "CPU",
    }
}

pub fn device_label(backend: GpuBackend, layers: u32) -> String {
    format!("{}, {layers} layers", device_name(backend))
}

/// Status text for the Trajectories panel. `layers` is the count actually loaded.
/// The label (`CUDA, 32 layers`) is a substring so the progress line does not print it twice.
pub fn device_message(backend: GpuBackend, layers: u32, fell_back: bool) -> String {
    if fell_back {
        let label = device_label(GpuBackend::Cpu, layers);
        format!("GPU runtime failed; classifying on {label}")
    } else {
        let label = device_label(backend, layers);
        match backend {
            GpuBackend::Cuda => format!("Running on {label}"),
            GpuBackend::Vulkan => format!("Running on {label}"),
            GpuBackend::Cpu => format!("No GPU found; classifying on {label}"),
        }
    }
}

/// Prompt plus 20–50 excerpt lines. Llama 3.1's native 131072 context is about 16 GB of KV cache,
/// which is the whole machine on a 16 GB Windows laptop. The process then prints an allocation
/// error and no classification JSON.
pub const CLASSIFY_CONTEXT: &str = "2048";
/// The classification object is a few hundred tokens. A long generation is prose, not a schema.
pub const CLASSIFY_PREDICT: &str = "384";

/// Llama 3.1 8B repeating layers. `-ngl` gets this count (or fewer when VRAM is known and tight).
pub const MODEL_LAYERS: u32 = 32;
const MIB_PER_LAYER: u64 = 160;
const RESERVE_MIB: u64 = 1536;

/// GPU layers to pass as `-ngl`.
///
/// CUDA and Vulkan always get a non-zero count. Unknown free VRAM offloads every layer:
/// `nvidia-smi` often prints nothing useful for free memory on Windows (WDDM), and treating
/// that as zero layers is what left VRAM flat. A GPU that cannot initialize is a failed
/// server start, and that is the only path to `-ngl 0`.
pub fn gpu_layers(backend: GpuBackend, free_mib: Option<u64>) -> u32 {
    if backend == GpuBackend::Cpu {
        return 0;
    }
    let Some(free) = free_mib else {
        return MODEL_LAYERS;
    };
    let budget = free.saturating_sub(RESERVE_MIB);
    ((budget / MIB_PER_LAYER) as u32).clamp(1, MODEL_LAYERS)
}

/// CPU is used when no GPU was found, or when the GPU backend failed to start.
/// A missing VRAM reading is not an init failure.
pub fn backend_after_init(preferred: GpuBackend, init_ok: bool) -> (GpuBackend, bool) {
    if init_ok || preferred == GpuBackend::Cpu {
        (preferred, false)
    } else {
        (GpuBackend::Cpu, true)
    }
}

/// mmap the GGUF and keep the context at 2048. Never mlock and never disable mmap.
///
/// `--fit` defaults to on and sizes unset context up to the trained length. `--fit-ctx` is the
/// smallest context that fit may use, not a maximum. Leaving fit on lets a 16 GB machine try to
/// allocate the 131072-token KV cache, fail, and print that error where the classification JSON
/// should be. Fit is turned off and `-c` is set instead.
pub fn memory_args(help: &str) -> Vec<String> {
    memory_args_with(help, false)
}

pub fn memory_args_with(help: &str, tight: bool) -> Vec<String> {
    let mut args = Vec::new();
    if help_has(help, "-c") || help_has(help, "--ctx-size") {
        args.push("-c".into());
        args.push(CLASSIFY_CONTEXT.into());
    }
    if help_has(help, "--load-mode") {
        args.push("--load-mode".into());
        args.push("mmap".into());
    } else if help_has(help, "--mmap") {
        args.push("--mmap".into());
    }
    if help_has(help, "--fit") {
        args.push("--fit".into());
        args.push("off".into());
    }
    if help_has(help, "-b") || help_has(help, "--batch-size") {
        args.push("-b".into());
        args.push(if tight { "128" } else { "512" }.into());
    }
    args
}

/// `-ngl` for a one-shot help check. The resident server always passes `-ngl` itself.
/// There is no tight retry that forces zero layers: that was the silent CPU fallback.
pub fn offload_args(backend: GpuBackend, help: &str) -> Vec<String> {
    offload_args_with(backend, help, None)
}

pub fn offload_args_with(backend: GpuBackend, help: &str, free_mib: Option<u64>) -> Vec<String> {
    let has_ngl = help_has(help, "-ngl") || help_has(help, "--n-gpu-layers");
    if !has_ngl {
        return Vec::new();
    }
    vec!["-ngl".into(), gpu_layers(backend, free_mib).to_string()]
}

/// Arguments for one long-lived `llama-server`. Context stays 2048 and `--fit off` so the KV
/// cache cannot grow toward the model's 131072-token default. `-ngl` is never `auto`.
pub fn server_args(backend: GpuBackend, help: &str, model: &Path, port: u16, free_mib: Option<u64>) -> Vec<String> {
    let mut args = vec!["-m".into(), model.display().to_string()];
    args.extend(memory_args(help));
    push_flag(&mut args, "-c", CLASSIFY_CONTEXT);
    push_flag(&mut args, "--fit", "off");
    let ngl = match free_mib {
        None => offload_args(backend, "-ngl"),
        Some(free) => offload_args_with(backend, "-ngl", Some(free)),
    };
    args.extend(ngl);
    push_flag(&mut args, "--host", "127.0.0.1");
    push_flag(&mut args, "--port", &port.to_string());
    if help_has(help, "-np") || help_has(help, "--parallel") {
        push_flag(&mut args, "-np", "1");
    }
    args
}

fn push_flag(args: &mut Vec<String>, flag: &str, value: &str) {
    if args.iter().any(|arg| arg == flag) {
        return;
    }
    args.push(flag.into());
    args.push(value.into());
}

/// JSON body for one classification. The prompt is the header sample; the grammar stops the
/// completion at the closing brace so `n_predict` is only a ceiling.
pub fn completion_request(system: &str, user: &str) -> Value {
    serde_json::json!({
        "prompt": llama_prompt(system, user),
        "n_predict": CLASSIFY_PREDICT.parse::<u64>().unwrap_or(384),
        "temperature": 0,
        "grammar": classification_grammar(),
        "cache_prompt": true,
        "stop": ["<|eot_id|>"],
    })
}

#[derive(Clone)]
struct ServerSpec {
    program: PathBuf,
    model: PathBuf,
    backend: GpuBackend,
    help: String,
    free_mib: Option<u64>,
    fell_back: bool,
    extra_env: Vec<(String, String)>,
}

struct ServerInfo {
    program: PathBuf,
    model: PathBuf,
    backend: GpuBackend,
    layers: u32,
    help: String,
    port: u16,
    fell_back: bool,
    free_mib: Option<u64>,
}

struct RunningServer {
    child: Child,
    port: u16,
    backend: GpuBackend,
    layers: u32,
    program: PathBuf,
    model: PathBuf,
    help: String,
    free_mib: Option<u64>,
    fell_back: bool,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr_thread: Option<thread::JoinHandle<()>>,
    stdout_thread: Option<thread::JoinHandle<()>>,
}

impl RunningServer {
    fn info(&self) -> ServerInfo {
        ServerInfo {
            program: self.program.clone(),
            model: self.model.clone(),
            backend: self.backend,
            layers: self.layers,
            help: self.help.clone(),
            port: self.port,
            fell_back: self.fell_back,
            free_mib: self.free_mib,
        }
    }
}

/// One llama-server process for the whole app session. A second classification reuses it.
pub struct LlamaServerPool {
    gate: Mutex<()>,
    slot: Mutex<Option<RunningServer>>,
    launches: AtomicU32,
}

impl LlamaServerPool {
    pub fn new() -> Self {
        Self {
            gate: Mutex::new(()),
            slot: Mutex::new(None),
            launches: AtomicU32::new(0),
        }
    }

    #[cfg(test)]
    pub fn launch_count(&self) -> u32 {
        self.launches.load(Ordering::SeqCst)
    }

    /// A healthy server, if one is already holding the model.
    fn current(&self) -> Option<ServerInfo> {
        let (port, info) = {
            let slot = self.slot.lock().unwrap_or_else(|err| err.into_inner());
            let running = slot.as_ref()?;
            (running.port, running.info())
        };
        health_ok(port).then_some(info)
    }

    pub fn shutdown(&self) {
        let _gate = self.gate.lock().unwrap_or_else(|err| err.into_inner());
        self.stop_slot();
    }

    /// Start `preferred` unless this model is already being served.
    /// A GPU that exits before `/health` starts `cpu_spec` instead. Cancel does not.
    fn start_or_fallback(
        &self,
        preferred: &ServerSpec,
        cpu_spec: impl FnOnce() -> Result<ServerSpec, String>,
        cancel: &AtomicBool,
    ) -> Result<ServerInfo, String> {
        if let Some(info) = self.current_for_model(&preferred.model) {
            return Ok(info);
        }
        if preferred.backend == GpuBackend::Cpu {
            return self.start(preferred, cancel);
        }
        match self.start(preferred, cancel) {
            Ok(info) => Ok(info),
            Err(err) if err == CANCELLED || cancel.load(Ordering::SeqCst) => Err(CANCELLED.into()),
            Err(gpu_err) => {
                let (fallback, fell_back) = backend_after_init(preferred.backend, false);
                let _ = fallback;
                if !fell_back {
                    return Err(gpu_err);
                }
                let mut cpu = cpu_spec()?;
                cpu.backend = GpuBackend::Cpu;
                cpu.fell_back = true;
                self.start(&cpu, cancel)
                    .map_err(|err| format!("{gpu_err}; CPU llama-server failed: {err}"))
            }
        }
    }

    fn infer(&self, spec: &ServerSpec, system: &str, user: &str, cancel: &AtomicBool) -> Result<String, String> {
        let info = self.start(spec, cancel)?;
        match post_completion(info.port, &completion_request(system, user), cancel) {
            Ok(text) => Ok(text),
            Err(err) if err == CANCELLED || cancel.load(Ordering::SeqCst) => Err(CANCELLED.into()),
            Err(err) => Err(err),
        }
    }

    fn current_for_model(&self, model: &Path) -> Option<ServerInfo> {
        let (port, info) = {
            let slot = self.slot.lock().unwrap_or_else(|err| err.into_inner());
            let running = slot.as_ref()?;
            if running.model.as_path() != model {
                return None;
            }
            (running.port, running.info())
        };
        health_ok(port).then_some(info)
    }

    fn start(&self, spec: &ServerSpec, cancel: &AtomicBool) -> Result<ServerInfo, String> {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let _gate = self.gate.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(info) = self.current_for_model(&spec.model) {
            return Ok(info);
        }
        self.stop_slot();
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let port = free_local_port()?;
        let running = spawn_server(spec, port)?;
        self.launches.fetch_add(1, Ordering::SeqCst);
        {
            let mut slot = self.slot.lock().unwrap_or_else(|err| err.into_inner());
            *slot = Some(running);
        }
        drop(_gate);
        self.wait_until_ready(port, cancel)?;
        self.slot
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .as_ref()
            .map(RunningServer::info)
            .ok_or_else(|| CANCELLED.into())
    }

    fn wait_until_ready(&self, port: u16, cancel: &AtomicBool) -> Result<(), String> {
        let started = Instant::now();
        loop {
            if cancel.load(Ordering::SeqCst) {
                self.shutdown();
                return Err(CANCELLED.into());
            }
            let exited = {
                let mut slot = self.slot.lock().unwrap_or_else(|err| err.into_inner());
                let Some(running) = slot.as_mut() else {
                    return Err(if cancel.load(Ordering::SeqCst) {
                        CANCELLED.into()
                    } else {
                        "llama-server stopped before it was ready".into()
                    });
                };
                if running.port != port {
                    return Err(CANCELLED.into());
                }
                match running.child.try_wait() {
                    Ok(Some(status)) => Some(status.code()),
                    Ok(None) => None,
                    Err(err) => return Err(format!("llama-server failed: {err}")),
                }
            };
            if let Some(code) = exited {
                let tail = self.copy_logs();
                self.shutdown();
                if cancel.load(Ordering::SeqCst) {
                    return Err(CANCELLED.into());
                }
                return Err(server_failure(&format!("llama-server exited ({code:?}): {tail}")));
            }
            if health_ok(port) {
                return Ok(());
            }
            if started.elapsed() >= SERVER_START_TIMEOUT {
                let tail = self.copy_logs();
                self.shutdown();
                return Err(format!("llama-server did not become ready: {tail}"));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn copy_logs(&self) -> String {
        let slot = self.slot.lock().unwrap_or_else(|err| err.into_inner());
        let Some(running) = slot.as_ref() else {
            return String::new();
        };
        let err = tail_text(&running.stderr);
        let out = tail_text(&running.stdout);
        format!("{err}\n{out}").trim().to_string()
    }

    fn stop_slot(&self) {
        let running = self.slot.lock().unwrap_or_else(|err| err.into_inner()).take();
        if let Some(running) = running {
            stop_running(running);
        }
    }
}

impl Drop for LlamaServerPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn tail_text(buf: &Mutex<Vec<u8>>) -> String {
    let guard = buf.lock().unwrap_or_else(|err| err.into_inner());
    let text = strip_ansi(&decode_process_text(&guard));
    let start = text.len().saturating_sub(1500);
    text[start..].trim().to_string()
}

fn stop_running(mut running: RunningServer) {
    let _ = running.child.kill();
    let _ = running.child.wait();
    if let Some(handle) = running.stderr_thread.take() {
        let _ = handle.join();
    }
    if let Some(handle) = running.stdout_thread.take() {
        let _ = handle.join();
    }
}

fn spawn_server(spec: &ServerSpec, port: u16) -> Result<RunningServer, String> {
    let layers = gpu_layers(spec.backend, spec.free_mib);
    let mut cmd = Command::new(&spec.program);
    cmd.args(server_args(spec.backend, &spec.help, &spec.model, port, spec.free_mib));
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = spec.program.parent() {
        cmd.current_dir(dir);
        #[cfg(unix)]
        {
            let dir = dir.to_string_lossy().to_string();
            let joined = match std::env::var("LD_LIBRARY_PATH") {
                Ok(existing) if !existing.is_empty() => format!("{dir}:{existing}"),
                _ => dir,
            };
            cmd.env("LD_LIBRARY_PATH", joined);
        }
    }
    for (key, value) in &spec.extra_env {
        cmd.env(key, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|err| format!("could not start llama-server: {err}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));
    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_for_thread = Arc::clone(&stderr_buf);
    let stdout_for_thread = Arc::clone(&stdout_buf);
    let stderr_thread = thread::spawn(move || drain_tail(stderr, stderr_for_thread));
    let stdout_thread = thread::spawn(move || drain_tail(stdout, stdout_for_thread));
    Ok(RunningServer {
        child,
        port,
        backend: spec.backend,
        layers,
        program: spec.program.clone(),
        model: spec.model.clone(),
        help: spec.help.clone(),
        free_mib: spec.free_mib,
        fell_back: spec.fell_back,
        stderr: stderr_buf,
        stdout: stdout_buf,
        stderr_thread: Some(stderr_thread),
        stdout_thread: Some(stdout_thread),
    })
}

fn drain_tail(mut reader: impl Read + Send, slot: Arc<Mutex<Vec<u8>>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut guard = slot.lock().unwrap_or_else(|err| err.into_inner());
                guard.extend_from_slice(&buf[..n]);
                if guard.len() > 8192 {
                    let drop_n = guard.len() - 8192;
                    guard.drain(..drop_n);
                }
            }
        }
    }
}

fn free_local_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| format!("could not reserve a port: {err}"))?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    drop(listener);
    Ok(port)
}

fn health_ok(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(200))
        .timeout_read(Duration::from_millis(500))
        .build();
    match agent.get(&url).call() {
        Ok(response) => response.into_string().unwrap_or_default().to_ascii_lowercase().contains("ok"),
        Err(_) => false,
    }
}

fn post_completion(port: u16, body: &Value, cancel: &AtomicBool) -> Result<String, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.into());
    }
    let url = format!("http://127.0.0.1:{port}/completion");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(INFERENCE_TIMEOUT)
        .build();
    let result = agent.post(&url).send_json(body);
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.into());
    }
    match result {
        Ok(response) => {
            let text = response.into_string().map_err(|err| err.to_string())?;
            completion_text(&text)
        }
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            Err(server_failure(&format!("HTTP {code}: {text}")))
        }
        Err(err) => Err(server_failure(&err.to_string())),
    }
}

fn server_failure(detail: &str) -> String {
    if runtime_failure(detail) {
        format!("llama-server could not initialize the model: {detail}")
    } else {
        format!("llama-server failed: {detail}")
    }
}

fn completion_text(body: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(body).map_err(|err| format!("llama-server returned non-JSON: {err}"))?;
    if let Some(content) = value.get("content").and_then(|item| item.as_str()) {
        if !content.trim().is_empty() {
            return Ok(content.to_string());
        }
    }
    if let Some(message) = value.pointer("/error/message").and_then(|item| item.as_str()) {
        return Err(format!("llama-server error: {message}"));
    }
    Err(format!("llama-server returned no completion: {body}"))
}

pub struct LlamaRuntime {
    pool: Arc<LlamaServerPool>,
    spec: ServerSpec,
    pub backend: GpuBackend,
    pub layers: u32,
    cancel: Arc<AtomicBool>,
}

impl LlamaRuntime {
    pub fn status_device(&self) -> String {
        device_label(self.backend, self.layers)
    }

    fn from_info(pool: Arc<LlamaServerPool>, info: ServerInfo, cancel: Arc<AtomicBool>) -> Self {
        let backend = info.backend;
        let layers = info.layers;
        let spec = ServerSpec {
            program: info.program,
            model: info.model,
            backend,
            help: info.help,
            free_mib: info.free_mib,
            fell_back: info.fell_back,
            extra_env: Vec::new(),
        };
        Self {
            pool,
            spec,
            backend,
            layers,
            cancel,
        }
    }

    /// Download the GGUF if needed and make sure one llama-server is serving it.
    /// A server that is already healthy is reused, including across later file picks.
    /// Call this only from a background task.
    pub fn ensure(
        data_dir: &Path,
        cancel: &Arc<AtomicBool>,
        progress: &dyn Fn(ClassifyNote),
        pool: &Arc<LlamaServerPool>,
    ) -> Result<Self, String> {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        if let Some(info) = pool.current() {
            let label = device_label(info.backend, info.layers);
            note(progress, &device_message(info.backend, info.layers, info.fell_back), 0, 0, &label);
            return Ok(Self::from_info(Arc::clone(pool), info, Arc::clone(cancel)));
        }
        let probed = probe_backend();
        note(progress, &format!("Preparing {} llama-server", device_name(probed)), 0, 0, device_name(probed));
        let model = ensure_model(data_dir, cancel, progress, probed)?;
        let (program, help, backend, binary_fell_back) = ensure_server(data_dir, probed, cancel, progress)?;
        let free_mib = if backend != GpuBackend::Cpu { free_vram_mib() } else { None };
        let layers = gpu_layers(backend, free_mib);
        let label = device_label(backend, layers);
        note(progress, &format!("Starting {label}"), 0, 0, &label);
        let spec = ServerSpec {
            program,
            model: model.clone(),
            backend,
            help,
            free_mib,
            fell_back: binary_fell_back,
            extra_env: Vec::new(),
        };
        let info = pool.start_or_fallback(
            &spec,
            || {
                let (program, help, backend, _) = ensure_server(data_dir, GpuBackend::Cpu, cancel, progress)?;
                Ok(ServerSpec {
                    program,
                    model,
                    backend,
                    help,
                    free_mib: None,
                    fell_back: true,
                    extra_env: Vec::new(),
                })
            },
            cancel,
        )?;
        let label = device_label(info.backend, info.layers);
        note(progress, &device_message(info.backend, info.layers, info.fell_back), 0, 0, &label);
        Ok(Self::from_info(Arc::clone(pool), info, Arc::clone(cancel)))
    }
}

fn note(progress: &dyn Fn(ClassifyNote), label: &str, bytes: u64, bytes_total: u64, device: &str) {
    progress(ClassifyNote {
        label: label.to_string(),
        bytes,
        bytes_total,
        device: device.to_string(),
    });
}

impl TextCompleter for LlamaRuntime {
    fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        self.pool.infer(&self.spec, system, user, &self.cancel)
    }
}

/// llama-cli interactive flags. The resident server does not use them; unknown flags would
/// make llama-server exit and look like a GPU init failure.
#[allow(dead_code)]
fn session_args(help: &str) -> Vec<String> {
    let mut args = Vec::new();
    if help_has(help, "--simple-io") {
        args.push("--simple-io".into());
    }
    if help_has(help, "--single-turn") {
        args.push("--single-turn".into());
    }
    if help_has(help, "--no-conversation") {
        args.push("--no-conversation".into());
    } else if help_has(help, "-no-cnv") {
        args.push("-no-cnv".into());
    }
    if help_has(help, "--no-display-prompt") {
        args.push("--no-display-prompt".into());
    }
    if help_has(help, "--no-show-timings") {
        args.push("--no-show-timings".into());
    }
    if help_has(help, "--skip-chat-parsing") {
        args.push("--skip-chat-parsing".into());
    }
    if help_has(help, "--reasoning") {
        args.push("--reasoning".into());
        args.push("off".into());
    }
    if help_has(help, "--temp") {
        args.push("--temp".into());
        args.push("0".into());
    } else if help_has(help, "--temperature") {
        args.push("--temperature".into());
        args.push("0".into());
    }
    args
}

/// The HTTP completion sends [`classification_grammar`] on every request. The JSON schema
/// converter in llama.cpp can reject this schema and exit with an error string, so it is not
/// sent. The text stays here as the shape the grammar is enforcing.
#[allow(dead_code)]
fn classification_schema() -> &'static str {
    JSON_SCHEMA
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

fn cli_help(cli: &Path, cancel: &AtomicBool) -> Result<String, String> {
    let mut cmd = Command::new(cli);
    cmd.arg("--help");
    let out = run_bounded(cmd, Duration::from_secs(30), cancel)?;
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Run a process until it exits, the cancel flag is set, or the timeout elapses.
pub fn run_bounded(mut cmd: Command, timeout: Duration, cancel: &AtomicBool) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|err| format!("could not start process: {err}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout_task = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_task = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_task.join();
            let _ = stderr_task.join();
            return Err(CANCELLED.into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_task.join();
                let _ = stderr_task.join();
                return Err("llama.cpp timed out".into());
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_task.join();
                let _ = stderr_task.join();
                return Err(format!("process failed: {err}"));
            }
        }
    };
    let stdout = stdout_task.join().unwrap_or_default();
    let stderr = stderr_task.join().unwrap_or_default();
    Ok(std::process::Output { status, stdout, stderr })
}

fn classification_grammar() -> &'static str {
    r#"root ::= "{" ws "\"header_lines\":" ws number ws "," ws "\"delimiter\":" ws string ws "," ws "\"frames\":" ws frames ws "," ws "\"units\":" ws units ws "," ws "\"columns\":" ws columns ws "," ws "\"confidence_score\":" ws number ws "," ws "\"unsupported_flag\":" ws boolean ws "," ws "\"reasoning\":" ws string ws "}"
ws ::= [ \t\n]*
number ::= "-"? [0-9]+ ("." [0-9]+)?
boolean ::= "true" | "false"
string ::= "\"" ([^"\\] | "\\" .)* "\""
frames ::= "{" ws "\"position\":" ws string (ws "," ws "\"velocity\":" ws string)? (ws "," ws "\"acceleration\":" ws string)? ws "}"
units ::= "{" ws "\"position\":" ws string (ws "," ws "\"velocity\":" ws string)? (ws "," ws "\"acceleration\":" ws string)? (ws "," ws "\"orientation\":" ws string)? (ws "," ws "\"mass\":" ws string)? ws "}"
columns ::= "{" ws pair (ws "," ws pair)* ws "}"
pair ::= string ws ":" ws string
"#
}

pub fn ensure_model(
    data_dir: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(ClassifyNote),
    backend: GpuBackend,
) -> Result<PathBuf, String> {
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
    let device = device_name(backend);
    download_to(GGUF_URL, &dest, cancel, &|status, bytes, total| {
        let label = if status.is_empty() {
            "Downloading Llama 3.1 8B Instruct"
        } else {
            status
        };
        note(progress, label, bytes, total, device);
    })
    .map_err(|err| {
        format!(
            "{err}. Llama-3.1-8B-Instruct Q4_K_M can also be placed at {}. Source: {GGUF_URL}",
            dest.display()
        )
    })?;
    Ok(dest)
}

fn server_override() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("FAUXRRT_LLAMA_SERVER") {
        return Some(PathBuf::from(path));
    }
    let Ok(cli) = std::env::var("FAUXRRT_LLAMA_CLI") else {
        return None;
    };
    let cli = PathBuf::from(cli);
    let name = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
    cli.parent().map(|dir| dir.join(name)).filter(|path| path.is_file())
}

/// llama-server binary, its `--help` text, the backend it will run, and whether the GPU
/// binary failed to launch (missing CUDA/Vulkan loader) before the model was loaded.
fn ensure_server(
    data_dir: &Path,
    probed: GpuBackend,
    cancel: &AtomicBool,
    progress: &dyn Fn(ClassifyNote),
) -> Result<(PathBuf, String, GpuBackend, bool), String> {
    if let Some(path) = server_override() {
        if !path.is_file() {
            return Err(format!("FAUXRRT_LLAMA_SERVER is not a file: {}", path.display()));
        }
        let help = cli_help(&path, cancel)?;
        // Keep the probed GPU even when --help is hard to parse. A binary that cannot
        // offload fails at server start, and that is the visible CPU fallback.
        return Ok((path, help, probed, false));
    }
    let primary = install_backend(data_dir, probed, cancel, progress)?;
    if probed != GpuBackend::Cpu && !cli_launches(&primary, cancel)? {
        let cpu = install_backend(data_dir, GpuBackend::Cpu, cancel, progress)?;
        let help = cli_help(&cpu, cancel)?;
        return Ok((cpu, help, GpuBackend::Cpu, true));
    }
    let help = cli_help(&primary, cancel)?;
    Ok((primary, help, probed, false))
}

fn install_backend(
    data_dir: &Path,
    backend: GpuBackend,
    cancel: &AtomicBool,
    progress: &dyn Fn(ClassifyNote),
) -> Result<PathBuf, String> {
    let root = data_dir.join("llama.cpp").join(backend_dir_name(backend));
    if let Some(found) = find_server(&root) {
        return Ok(found);
    }
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let (name, url) = cli_asset(backend);
    let device = device_name(backend);
    let archive = root.join(&name);
    download_to(&url, &archive, cancel, &|status, bytes, total| {
        let label = if status.is_empty() {
            format!("Downloading llama.cpp ({device})")
        } else {
            status.to_string()
        };
        note(progress, &label, bytes, total, device);
    })?;
    extract_archive(&archive, &root, cancel)?;
    find_server(&root).ok_or_else(|| format!("llama.cpp archive did not contain llama-server ({url})"))
}

pub fn cli_asset(backend: GpuBackend) -> (String, String) {
    let name = if cfg!(windows) {
        match backend {
            GpuBackend::Cuda => format!("llama-{LLAMA_TAG}-bin-win-cuda-12.4-x64.zip"),
            GpuBackend::Vulkan => format!("llama-{LLAMA_TAG}-bin-win-vulkan-x64.zip"),
            GpuBackend::Cpu => format!("llama-{LLAMA_TAG}-bin-win-cpu-x64.zip"),
        }
    } else {
        match backend {
            GpuBackend::Cuda => format!("llama-{LLAMA_TAG}-bin-ubuntu-cuda-12.8-x64.tar.gz"),
            GpuBackend::Vulkan => format!("llama-{LLAMA_TAG}-bin-ubuntu-vulkan-x64.tar.gz"),
            GpuBackend::Cpu => format!("llama-{LLAMA_TAG}-bin-ubuntu-x64.tar.gz"),
        }
    };
    let url = format!("{LLAMA_RELEASE}/{name}");
    (name, url)
}

fn backend_dir_name(backend: GpuBackend) -> &'static str {
    match backend {
        GpuBackend::Cuda => "cuda",
        GpuBackend::Vulkan => "vulkan",
        GpuBackend::Cpu => "cpu",
    }
}

fn probe_backend() -> GpuBackend {
    choose_backend(nvidia_present(), vulkan_present())
}

fn nvidia_present() -> bool {
    process_succeeds("nvidia-smi", &["-L"], Duration::from_secs(3))
}

fn vulkan_present() -> bool {
    if cfg!(windows) && windows_vulkan_loader() {
        return true;
    }
    process_succeeds("vulkaninfo", &["--summary"], Duration::from_secs(4))
}

fn windows_vulkan_loader() -> bool {
    let root = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("WINDIR"));
    let Some(root) = root else {
        return false;
    };
    PathBuf::from(root).join("System32").join("vulkan-1.dll").is_file()
}

fn process_succeeds(program: &str, args: &[&str], timeout: Duration) -> bool {
    let mut cmd = Command::new(program);
    cmd.args(args);
    match run_bounded(cmd, timeout, &AtomicBool::new(false)) {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

fn cli_launches(path: &Path, cancel: &AtomicBool) -> Result<bool, String> {
    let mut cmd = Command::new(path);
    cmd.arg("--version");
    match run_bounded(cmd, Duration::from_secs(30), cancel) {
        Err(err) if err == CANCELLED => Err(err),
        Err(_) => Ok(false),
        Ok(out) => Ok(out.status.success()),
    }
}

fn find_server(root: &Path) -> Option<PathBuf> {
    let names = ["llama-server", "llama-server.exe"];
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .find(|entry| names.iter().any(|name| entry.file_name().to_string_lossy().eq_ignore_ascii_case(name)))
        .map(|entry| entry.into_path())
}

const DOWNLOAD_ATTEMPTS: u32 = 8;

fn retry_pause(attempt: u32) -> Duration {
    Duration::from_millis(400u64.saturating_mul(1u64 << attempt.min(4)))
}

fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dest.with_file_name(name)
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn expects_gguf(dest: &Path) -> bool {
    dest.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
}

fn already_complete(dest: &Path) -> bool {
    let len = file_len(dest);
    len > 0 && (!expects_gguf(dest) || len >= GGUF_MIN_BYTES)
}

pub fn retryable_download_error(err: &str) -> bool {
    if err == CANCELLED || err.contains(CANCELLED) {
        return false;
    }
    let lower = err.to_ascii_lowercase();
    if lower.contains("smaller than") {
        return false;
    }
    if lower.contains("status code 4") && !lower.contains("status code 408") && !lower.contains("status code 429") {
        return false;
    }
    lower.contains("connection")
        || lower.contains("forcibly closed")
        || lower.contains("10054")
        || lower.contains("reset")
        || lower.contains("broken pipe")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("tls")
        || lower.contains("eof")
        || lower.contains("read download")
        || lower.contains("retry-download")
}

fn content_range_total(value: &str) -> Option<u64> {
    let total = value.split('/').nth(1)?.trim();
    if total == "*" {
        None
    } else {
        total.parse().ok()
    }
}

fn content_range_start(value: &str) -> Option<u64> {
    let spec = value.split_whitespace().nth(1)?;
    spec.split('-').next()?.parse().ok()
}

fn wait_retry(pause: Duration, cancel: &AtomicBool) -> Result<(), String> {
    let step = Duration::from_millis(50);
    let mut left = pause;
    while !left.is_zero() {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let slice = left.min(step);
        thread::sleep(slice);
        left = left.saturating_sub(slice);
    }
    if cancel.load(Ordering::SeqCst) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

fn adopt_incomplete_dest(dest: &Path, partial: &Path) -> Result<(), String> {
    if !dest.is_file() || already_complete(dest) {
        return Ok(());
    }
    if file_len(dest) > file_len(partial) {
        if partial.exists() {
            fs::remove_file(partial).ok();
        }
        fs::rename(dest, partial).map_err(|err| format!("keep partial download: {err}"))?;
    }
    Ok(())
}

fn download_to(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    on_status: &dyn Fn(&str, u64, u64),
) -> Result<(), String> {
    download_with(url, dest, cancel, DOWNLOAD_ATTEMPTS, &retry_pause, on_status)
}

fn download_with(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    attempts: u32,
    pause: &dyn Fn(u32) -> Duration,
    on_status: &dyn Fn(&str, u64, u64),
) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.into());
    }
    if already_complete(dest) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let partial = partial_path(dest);
    adopt_incomplete_dest(dest, &partial)?;
    let last_total = Cell::new(0u64);
    let mut last_err = String::new();
    let attempts = attempts.max(1);
    for attempt in 0..attempts {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let report = |status: &str, bytes: u64, total: u64| {
            if total > 0 {
                last_total.set(total);
            }
            let shown = if total > 0 { total } else { last_total.get() };
            on_status(status, bytes, shown);
        };
        match download_once(url, dest, &partial, cancel, &report) {
            Ok(()) => return Ok(()),
            Err(err) if err == CANCELLED || err.contains(CANCELLED) => return Err(err),
            Err(err) if retryable_download_error(&err) && attempt + 1 < attempts => {
                last_err = err;
                report("Download interrupted; resuming", file_len(&partial), last_total.get());
                wait_retry(pause(attempt), cancel)?;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_err)
}

fn download_once(
    url: &str,
    dest: &Path,
    partial: &Path,
    cancel: &AtomicBool,
    on_status: &dyn Fn(&str, u64, u64),
) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.into());
    }
    let have = file_len(partial);
    on_status("", have, 0);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(90))
        .timeout(Duration::from_secs(60 * 180))
        .build();
    let request = if have > 0 {
        agent.get(url).set("Range", &format!("bytes={have}-"))
    } else {
        agent.get(url)
    };
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(416, _)) if have > 0 => {
            fs::remove_file(partial).ok();
            return Err(format!("download {url} failed: range was rejected; retry-download"));
        }
        Err(err) => return Err(format!("download {url} failed: {err}")),
    };
    let status = response.status();
    let content_range = response.header("content-range").unwrap_or("").to_string();
    let content_length = response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let (append, total) = if status == 206 {
        let start = content_range_start(&content_range).unwrap_or(have);
        if start != have {
            return Err(format!("download {url} failed: range was rejected; retry-download"));
        }
        let total = content_range_total(&content_range).unwrap_or(have.saturating_add(content_length));
        (true, total)
    } else {
        (false, content_length)
    };
    if expects_gguf(dest) && total > 0 && total < GGUF_MIN_BYTES {
        return Err(format!(
            "download {url} failed: file is {total} bytes, smaller than Llama-3.1-8B-Instruct Q4_K_M"
        ));
    }
    let mut file = if append {
        OpenOptions::new().create(true).append(true).open(partial)
    } else {
        OpenOptions::new().create(true).write(true).truncate(true).open(partial)
    }
    .map_err(|err| format!("create {}: {err}", partial.display()))?;
    let base = if append { have } else { 0 };
    on_status("", base, total);
    let last_emit = Cell::new(Instant::now() - Duration::from_secs(1));
    let mut reader = response.into_reader();
    let copied = copy_cancellable(&mut reader, &mut file, cancel, &|bytes| {
        let done = base + bytes;
        if last_emit.get().elapsed() >= Duration::from_millis(200) || (total > 0 && done >= total) {
            on_status("", done, total);
            last_emit.set(Instant::now());
        }
    });
    file.flush().ok();
    let written = match copied {
        Ok(n) => n,
        Err(err) => return Err(err),
    };
    let end = base + written;
    if total > 0 && end < total {
        return Err(format!("download {url} failed: connection closed before the file finished"));
    }
    if expects_gguf(dest) && end < GGUF_MIN_BYTES {
        return Err(format!(
            "download {url} failed: connection closed before Llama-3.1-8B-Instruct Q4_K_M finished"
        ));
    }
    file.sync_all().ok();
    drop(file);
    if dest.exists() {
        fs::remove_file(dest).ok();
    }
    fs::rename(partial, dest).map_err(|err| format!("rename download: {err}"))?;
    Ok(())
}

pub fn copy_cancellable<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    cancel: &AtomicBool,
    on_progress: &dyn Fn(u64),
) -> Result<u64, String> {
    let mut buf = [0u8; 1024 * 256];
    let mut written = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let n = reader.read(&mut buf).map_err(|err| format!("read download: {err}"))?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).map_err(|err| format!("write download: {err}"))?;
        written += n as u64;
        on_progress(written);
    }
    Ok(written)
}

fn extract_archive(archive: &Path, dest: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let cmd = if name.ends_with(".zip") {
        if cfg!(windows) {
            let mut cmd = Command::new("powershell");
            cmd.args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -Force -LiteralPath '{}' -DestinationPath '{}'",
                    archive.display(),
                    dest.display()
                ),
            ]);
            cmd
        } else {
            let mut cmd = Command::new("unzip");
            cmd.args(["-o", &archive.to_string_lossy(), "-d", &dest.to_string_lossy()]);
            cmd
        }
    } else {
        let mut cmd = Command::new("tar");
        cmd.args(["-xzf", &archive.to_string_lossy(), "-C", &dest.to_string_lossy()]);
        cmd
    };
    let output = run_bounded(cmd, Duration::from_secs(10 * 60), cancel)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("extract {} failed", archive.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    fn sample_ecef() -> TrajectoryClassification {
        let mut columns = BTreeMap::new();
        columns.insert("col_0".into(), "time".into());
        columns.insert("col_1".into(), "pos_x".into());
        columns.insert("col_2".into(), "pos_y".into());
        columns.insert("col_3".into(), "pos_z".into());
        TrajectoryClassification {
            header_lines: 1,
            delimiter: ",".into(),
            frames: ChannelFrames {
                position: "ECEF".into(),
                velocity: None,
                acceleration: None,
            },
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
            "frames",
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
        assert!(SYSTEM_PROMPT.contains("x-ecr") && SYSTEM_PROMPT.contains("y-ecr") && SYSTEM_PROMPT.contains("z-ecr"));
        assert!(SYSTEM_PROMPT.contains("north, east, and down"));
        assert!(SYSTEM_PROMPT.contains("Do not convert coordinates"));
        assert!(SYSTEM_PROMPT.contains("frames.position"));
        assert!(SYSTEM_PROMPT.contains("frames.velocity"));
        assert!(SYSTEM_PROMPT.contains("frames.acceleration"));
        assert!(!SYSTEM_PROMPT.contains("\"coordinate_system\""));
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
        assert_eq!(class.frames.position, "ECEF");
        assert!(class.frames.velocity.is_none());
        assert!(class.frames.acceleration.is_none());
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
        let old = r#"{"header_lines":1,"delimiter":",","coordinate_system":"ECEF","units":{"position":"m","velocity":"m/s"},"columns":{"col_0":"time","col_1":"pos_x","col_2":"pos_y","col_3":"pos_z","col_4":"vel_x","col_5":"vel_y","col_6":"vel_z"},"confidence_score":0.9,"unsupported_flag":false,"reasoning":"old file"}"#;
        let legacy = accept_output(old).unwrap();
        assert_eq!(legacy.frames.position, "ECEF");
        assert_eq!(legacy.frames.velocity.as_deref(), Some("ECEF"));
        assert!(legacy.frames.acceleration.is_none());
    }

    #[test]
    fn unsupported_frame_and_low_confidence_need_manual_assignment() {
        let mut class = sample_ecef();
        class.frames.position = "BODY".into();
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
        class.frames.position = "NED".into();
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
        let model = match std::env::var("FAUXRRT_LLAMA_MODEL") {
            Ok(path) if Path::new(&path).is_file() => path,
            _ => return,
        };
        let server = match std::env::var("FAUXRRT_LLAMA_SERVER") {
            Ok(path) if Path::new(&path).is_file() => path,
            _ => return,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let help = cli_help(Path::new(&server), &cancel).unwrap_or_default();
        let backend = probe_backend();
        let pool = Arc::new(LlamaServerPool::new());
        let spec = ServerSpec {
            program: PathBuf::from(server),
            model: PathBuf::from(&model),
            backend,
            help: help.clone(),
            free_mib: if backend != GpuBackend::Cpu { free_vram_mib() } else { None },
            fell_back: false,
            extra_env: Vec::new(),
        };
        let info = pool
            .start_or_fallback(
                &spec,
                || {
                    Ok(ServerSpec {
                        backend: GpuBackend::Cpu,
                        free_mib: None,
                        fell_back: true,
                        help,
                        ..spec.clone()
                    })
                },
                &cancel,
            )
            .expect("live llama-server");
        let runtime = LlamaRuntime::from_info(Arc::clone(&pool), info, cancel);
        let excerpt = "time,lat,lon,alt\n0,32.4,-106.4,1000\n1,32.41,-106.39,1100\n";
        let class = classify_with(&runtime, excerpt).expect("live classification");
        assert_eq!(class.frames.position, "LLA");
        assert!(has_position_columns(&class));
        pool.shutdown();
    }

    #[test]
    fn gpu_preference_is_cuda_then_vulkan_then_cpu() {
        assert_eq!(choose_backend(true, true), GpuBackend::Cuda);
        assert_eq!(choose_backend(true, false), GpuBackend::Cuda);
        assert_eq!(choose_backend(false, true), GpuBackend::Vulkan);
        assert_eq!(choose_backend(false, false), GpuBackend::Cpu);
        assert_eq!(
            device_message(GpuBackend::Cpu, 0, false),
            "No GPU found; classifying on CPU, 0 layers"
        );
        assert_eq!(device_message(GpuBackend::Cuda, 32, false), "Running on CUDA, 32 layers");
        assert_eq!(device_message(GpuBackend::Vulkan, 32, false), "Running on Vulkan, 32 layers");
        assert_eq!(
            device_message(GpuBackend::Cuda, 0, true),
            "GPU runtime failed; classifying on CPU, 0 layers"
        );
        let label = device_label(GpuBackend::Cuda, 32);
        assert!(device_message(GpuBackend::Cuda, 32, false).contains(&label));
        assert_eq!(label, "CUDA, 32 layers");

        let (cuda_name, cuda_url) = cli_asset(GpuBackend::Cuda);
        let (vulkan_name, vulkan_url) = cli_asset(GpuBackend::Vulkan);
        let (cpu_name, cpu_url) = cli_asset(GpuBackend::Cpu);
        assert!(cuda_url.contains("b11538") && cuda_name.contains("cuda"));
        assert!(vulkan_name.contains("vulkan") && vulkan_url.contains("b11538"));
        assert!(!cpu_name.contains("cuda") && !cpu_name.contains("vulkan"));
        assert!(cpu_url.contains("b11538"));
        if cfg!(windows) {
            assert!(cuda_name.contains("win-cuda-12.4"));
            assert!(vulkan_name.contains("win-vulkan"));
            assert!(cpu_name.ends_with(".zip"));
        } else {
            assert!(cuda_name.contains("ubuntu-cuda"));
            assert!(cpu_name.ends_with(".tar.gz"));
        }
    }

    #[test]
    fn gpu_offload_is_nonzero_unless_the_backend_is_cpu() {
        let help = "usage --fit [on|off] -ngl N --n-gpu-layers N --temp N";
        let cuda = offload_args(GpuBackend::Cuda, help);
        assert_eq!(cuda, vec!["-ngl".to_string(), "32".to_string()]);
        assert!(!cuda.iter().any(|arg| arg == "auto" || arg == "all" || arg == "on" || arg == "0"));
        assert_eq!(offload_args(GpuBackend::Cpu, help), vec!["-ngl".to_string(), "0".to_string()]);
        assert!(offload_args(GpuBackend::Cuda, "usage --temp N").is_empty());
        let capped = offload_args_with(GpuBackend::Cuda, "usage -ngl N", Some(8_192));
        assert_eq!(capped, vec!["-ngl".to_string(), "32".to_string()]);
        assert_eq!(
            offload_args_with(GpuBackend::Vulkan, "usage -ngl N", None),
            vec!["-ngl".to_string(), "32".to_string()]
        );
        assert_eq!(
            offload_args_with(GpuBackend::Cuda, "usage -ngl N", Some(2_000)),
            vec!["-ngl".to_string(), "2".to_string()]
        );
        assert_eq!(gpu_layers(GpuBackend::Cuda, None), 32);
        assert_eq!(gpu_layers(GpuBackend::Vulkan, None), 32);
        assert_eq!(gpu_layers(GpuBackend::Cpu, None), 0);
        assert_eq!(gpu_layers(GpuBackend::Cpu, Some(16_000)), 0);
        assert_eq!(gpu_layers(GpuBackend::Cuda, Some(2_000)), 2);
        assert_eq!(gpu_layers(GpuBackend::Cuda, Some(1_000)), 1);
    }

    #[test]
    fn cpu_fallback_happens_only_after_gpu_init_fails() {
        assert_eq!(backend_after_init(GpuBackend::Cuda, true), (GpuBackend::Cuda, false));
        assert_eq!(backend_after_init(GpuBackend::Vulkan, true), (GpuBackend::Vulkan, false));
        assert_eq!(backend_after_init(GpuBackend::Cpu, true), (GpuBackend::Cpu, false));
        assert_eq!(backend_after_init(GpuBackend::Cuda, false), (GpuBackend::Cpu, true));
        assert_eq!(backend_after_init(GpuBackend::Vulkan, false), (GpuBackend::Cpu, true));
        assert!(gpu_layers(GpuBackend::Cuda, None) > 0);
        assert!(gpu_layers(GpuBackend::Vulkan, None) > 0);
    }

    #[test]
    fn llama_server_launch_keeps_context_capped_and_offloads_layers() {
        let help = "usage -c N --mmap --fit [on|off] -b N -ngl N --host --port -np N";
        let cuda = server_args(GpuBackend::Cuda, help, Path::new("model.gguf"), 8081, None);
        assert!(cuda.windows(2).any(|pair| pair == ["-m", "model.gguf"]), "{cuda:?}");
        assert!(cuda.windows(2).any(|pair| pair == ["-c", "2048"]), "{cuda:?}");
        assert!(cuda.windows(2).any(|pair| pair == ["--fit", "off"]), "{cuda:?}");
        assert!(cuda.windows(2).any(|pair| pair == ["-ngl", "32"]), "{cuda:?}");
        assert!(cuda.windows(2).any(|pair| pair == ["--host", "127.0.0.1"]), "{cuda:?}");
        assert!(cuda.windows(2).any(|pair| pair == ["--port", "8081"]), "{cuda:?}");
        assert!(!cuda.iter().any(|arg| arg == "auto" || arg == "131072"));
        let vulkan = server_args(GpuBackend::Vulkan, help, Path::new("model.gguf"), 9, None);
        assert!(vulkan.windows(2).any(|pair| pair == ["-ngl", "32"]), "{vulkan:?}");
        let cpu = server_args(GpuBackend::Cpu, help, Path::new("model.gguf"), 9, Some(16_000));
        assert!(cpu.windows(2).any(|pair| pair == ["-ngl", "0"]), "{cpu:?}");
        assert!(cpu.windows(2).any(|pair| pair == ["-c", "2048"]), "{cpu:?}");
    }

    #[test]
    fn completion_request_sends_the_header_sample_and_grammar() {
        let excerpt = "time,lat,lon,alt\n0,32.4,-106.4,1000\n";
        let body = completion_request(SYSTEM_PROMPT, &user_message(excerpt));
        assert_eq!(body["n_predict"], 384);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["cache_prompt"], true);
        let prompt = body["prompt"].as_str().unwrap();
        assert!(prompt.contains("time,lat,lon,alt"));
        assert!(prompt.contains(excerpt.trim()));
        let grammar = body["grammar"].as_str().unwrap();
        assert!(grammar.contains("header_lines"));
        assert!(grammar.contains("frames"));
        assert!(classification_schema().contains("header_lines"));
        assert!(!prompt.contains("131072"));
    }

    #[test]
    fn llama_context_is_small_and_weights_stay_mmapd() {
        let help = "usage -c --ctx-size N --mmap --no-mmap --load-mode --fit --fit-ctx N -b --batch-size N --json-schema-file F --grammar-file F";
        let args = memory_args(help);
        assert!(args.windows(2).any(|pair| pair == ["-c", "2048"]), "{args:?}");
        assert!(args.windows(2).any(|pair| pair == ["--load-mode", "mmap"]), "{args:?}");
        assert!(args.windows(2).any(|pair| pair == ["--fit", "off"]), "{args:?}");
        assert!(args.windows(2).any(|pair| pair == ["-b", "512"]), "{args:?}");
        assert!(!args.iter().any(|arg| arg == "--fit-ctx" || arg == "on"));
        assert!(!args.iter().any(|arg| arg == "--no-mmap" || arg == "--mlock" || arg == "mlock"));
        let mmap_only = memory_args("usage -c N --mmap --no-mmap");
        assert!(mmap_only.iter().any(|arg| arg == "--mmap"), "{mmap_only:?}");
        assert!(!mmap_only.iter().any(|arg| arg.contains("no-mmap") || arg.contains("mlock")));
        let tight = memory_args_with(help, true);
        assert!(tight.windows(2).any(|pair| pair == ["-b", "128"]), "{tight:?}");
    }

    #[test]
    fn subprocess_io_stays_on_the_pipe_and_reasoning_stays_off() {
        let help = "usage --simple-io --single-turn --no-conversation --no-display-prompt --no-show-timings --skip-chat-parsing --reasoning [on|off|auto] --temp N";
        let args = session_args(help);
        for flag in ["--simple-io", "--single-turn", "--no-conversation", "--no-display-prompt", "--no-show-timings", "--skip-chat-parsing"] {
            assert!(args.iter().any(|arg| arg == flag), "{flag} missing from {args:?}");
        }
        assert!(args.windows(2).any(|pair| pair == ["--reasoning", "off"]), "{args:?}");
        assert!(args.windows(2).any(|pair| pair == ["--temp", "0"]), "{args:?}");
    }

    #[test]
    fn allocation_error_is_not_a_classification_and_utf16_json_is() {
        let err = "llama_kv_cache_init: failed to allocate buffer for kv cache\nmain: error: unable to load model\n";
        assert!(runtime_failure(err));
        assert!(accept_output(err).unwrap_err().contains("not a JSON object"));
        let json = r#"{"header_lines":1,"delimiter":",","frames":{"position":"LLA"},"units":{"position":"m"},"columns":{"col_0":"time","col_1":"pos_lat","col_2":"pos_lon","col_3":"pos_alt"},"confidence_score":0.9,"unsupported_flag":false,"reasoning":"lat lon alt"}"#;
        let mut utf16 = Vec::new();
        for unit in json.encode_utf16() {
            utf16.extend(unit.to_le_bytes());
        }
        let decoded = decode_process_text(&utf16);
        let class = accept_output(&decoded).unwrap();
        assert_eq!(class.frames.position, "LLA");
        let noisy = format!("log line without an object\n{json}");
        assert_eq!(accept_output(&noisy).unwrap().columns.get("col_1").map(String::as_str), Some("pos_lat"));
        let example = r#"{"header_lines":1,"delimiter":",","frames":{"position":"ECEF","velocity":"NED"},"units":{"position":"m","velocity":"m/s"},"columns":{"col_0":"time","col_1":"pos_x"},"confidence_score":0.85,"unsupported_flag":false,"reasoning":"example"}"#;
        let both = format!("{example}\n{json}");
        assert_eq!(accept_output(&both).unwrap().frames.position, "LLA");
        assert!(!runtime_failure("classification was not the JSON object"));
    }

    #[test]
    fn every_excerpt_including_named_headers_is_sent_to_the_model() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("samples")
            .join("eglin_keywest_6dof.csv");
        let text = fs::read_to_string(&path).unwrap();
        let excerpt = excerpt_of(&text);
        assert!(excerpt.lines().count() <= 50 && excerpt.lines().count() >= 20);
        assert!(excerpt.starts_with("time,lat,lon,alt"));
        let json = r#"{"header_lines":1,"delimiter":",","frames":{"position":"LLA"},"units":{"position":"m"},"columns":{"col_0":"time","col_1":"pos_lat","col_2":"pos_lon","col_3":"pos_alt"},"confidence_score":0.93,"unsupported_flag":false,"reasoning":"lat lon alt"}"#;
        let saw = Mutex::new(String::new());
        struct Watch<'a> {
            json: &'a str,
            saw: &'a Mutex<String>,
            calls: Mutex<usize>,
        }
        impl TextCompleter for Watch<'_> {
            fn complete(&self, system: &str, user: &str) -> Result<String, String> {
                *self.calls.lock().unwrap() += 1;
                assert!(system.contains("x-ecr"));
                assert!(system.contains("Do not convert coordinates"));
                *self.saw.lock().unwrap() = user.to_string();
                Ok(self.json.to_string())
            }
        }
        let model = Watch { json, saw: &saw, calls: Mutex::new(0) };
        let class = classify_with(&model, &excerpt).unwrap();
        assert_eq!(*model.calls.lock().unwrap(), 1);
        assert!(saw.lock().unwrap().contains("time,lat,lon,alt"));
        assert_eq!(class.header_lines, 1);
        assert_eq!(class.delimiter, ",");
        assert_eq!(class.frames.position, "LLA");
        assert_eq!(class.columns.get("col_1").map(String::as_str), Some("pos_lat"));
        assert!(class.units.velocity.is_none());
        assert!(may_load(&class, false), "{class:?}");
        let track = crate::parse::parse_with_classification(&text, &class, None).unwrap();
        assert!(track.lla.len() / 3 >= 300);
        assert!((track.lla[0] as f64 + 86.5254).abs() < 1e-2);
        assert!((track.lla[1] as f64 - 30.4832).abs() < 1e-2);

        let ecr = "time,x-ecr,z-ecr,y-ecr\n0,1,2,3\n";
        let ned = "time,north,east,down\n0,1,2,3\n";
        for excerpt in [ecr, ned] {
            let model = Watch { json, saw: &saw, calls: Mutex::new(0) };
            let _ = classify_with(&model, excerpt).unwrap();
            assert_eq!(*model.calls.lock().unwrap(), 1);
            assert!(saw.lock().unwrap().contains(excerpt.lines().next().unwrap()));
        }
    }

    #[test]
    fn stuck_llama_times_out_and_cancel_kills_it() {
        let cancel = AtomicBool::new(false);
        let err = run_bounded(slow_command(), Duration::from_millis(400), &cancel).unwrap_err();
        assert!(err.contains("timed out"), "{err}");

        cancel.store(true, Ordering::SeqCst);
        let err = run_bounded(slow_command(), Duration::from_secs(30), &cancel).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");

        let out = run_bounded(echo_command(), Duration::from_secs(5), &AtomicBool::new(false)).unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).to_ascii_lowercase().contains("ok"));
    }

    #[test]
    fn download_copy_stops_when_cancelled() {
        let cancel = AtomicBool::new(false);
        let data = vec![7u8; 1024 * 256 * 3];
        let mut reader = std::io::Cursor::new(data);
        let mut writer = Vec::new();
        let err = copy_cancellable(&mut reader, &mut writer, &cancel, &|n| {
            if n >= 1024 * 256 {
                cancel.store(true, Ordering::SeqCst);
            }
        })
        .unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert!(writer.len() < 1024 * 256 * 3);
        assert!(!writer.is_empty());
    }

    #[test]
    fn connection_reset_is_retried_and_a_missing_file_is_not() {
        let seen = "download https://huggingface.co/bartowski/Meta-Llama-3.1-8B-Instruct-GGUF/resolve/main/Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf failed: Connection Failed: tls connection init failed: An existing connection was forcibly closed by the remote host. (os error 10054)";
        assert!(retryable_download_error(seen), "{seen}");
        assert!(retryable_download_error("download http://example failed: connection closed before the file finished"));
        assert!(!retryable_download_error("download http://example failed: example: status code 404"));
        assert!(!retryable_download_error("classification cancelled"));
        assert!(!retryable_download_error(
            "download http://example failed: file is 12 bytes, smaller than Llama-3.1-8B-Instruct Q4_K_M"
        ));
    }

    #[test]
    fn download_resumes_after_the_connection_drops() {
        let body: Vec<u8> = (0..180u8).map(|i| i.wrapping_mul(3)).collect();
        let server = TestDownload::serve(body.clone(), DownloadScript::CutFirst { bytes: 40 });
        let dest = temp_download("resume.zip");
        download_with(&server.url, &dest, &AtomicBool::new(false), 4, &|_| Duration::ZERO, &|_, _, _| {}).unwrap();
        let saved = fs::read(&dest).unwrap();
        assert_eq!(saved, body);
        let requests = server.requests.lock().unwrap();
        assert!(requests.len() >= 2, "{requests:?}");
        assert!(requests[1].to_ascii_lowercase().contains("range: bytes=40-"), "{}", requests[1]);
    }

    #[test]
    fn download_keeps_a_partial_when_every_attempt_resets() {
        let body = vec![9u8; 80];
        let server = TestDownload::serve(body, DownloadScript::CutEvery { bytes: 16 });
        let dest = temp_download("stuck.zip");
        let err = download_with(&server.url, &dest, &AtomicBool::new(false), 3, &|_| Duration::ZERO, &|status, _, _| {
            if !status.is_empty() {
                assert!(status.contains("interrupted"), "{status}");
            }
        })
        .unwrap_err();
        assert!(retryable_download_error(&err), "{err}");
        assert!(!dest.is_file());
        let partial = partial_path(&dest);
        assert!(file_len(&partial) > 0, "partial {}", file_len(&partial));
    }

    #[test]
    fn existing_partial_is_continued_with_range() {
        let body: Vec<u8> = (0..90).map(|i| 255 - i).collect();
        let server = TestDownload::serve(body.clone(), DownloadScript::Whole);
        let dest = temp_download("continue.zip");
        let partial = partial_path(&dest);
        fs::write(&partial, &body[..30]).unwrap();
        download_with(&server.url, &dest, &AtomicBool::new(false), 2, &|_| Duration::ZERO, &|_, _, _| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), body);
        let requests = server.requests.lock().unwrap();
        assert!(requests[0].to_ascii_lowercase().contains("range: bytes=30-"), "{}", requests[0]);
    }

    #[test]
    fn editor_guess_stays_a_guess_when_the_model_does_not_answer() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("samples")
            .join("eglin_keywest_6dof.csv");
        let text = fs::read_to_string(&path).unwrap();
        let class = partial_schema_guess(&text);
        assert!(class.confidence_score <= 0.2, "{}", class.confidence_score);
        assert!(!may_load(&class, false), "a header guess must not load as the model result");
    }

    fn temp_download(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fauxrrt-dl-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[derive(Clone, Copy)]
    enum DownloadScript {
        Whole,
        CutFirst { bytes: usize },
        CutEvery { bytes: usize },
    }

    struct TestDownload {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl TestDownload {
        fn serve(body: Vec<u8>, script: DownloadScript) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let requests_bg = Arc::clone(&requests);
            let stop_bg = Arc::clone(&stop);
            let join = thread::spawn(move || {
                let mut served = 0usize;
                while !stop_bg.load(Ordering::SeqCst) && served < 8 {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            served += 1;
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            let mut buf = [0u8; 4096];
                            let n = stream.read(&mut buf).unwrap_or(0);
                            let req = String::from_utf8_lossy(&buf[..n]).to_string();
                            requests_bg.lock().unwrap().push(req.clone());
                            let start = header_range_start(&req).unwrap_or(0) as usize;
                            if start > body.len() {
                                let _ = stream.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                                continue;
                            }
                            let slice = &body[start..];
                            let cut = match script {
                                DownloadScript::Whole => None,
                                DownloadScript::CutFirst { bytes } if served == 1 => Some(bytes),
                                DownloadScript::CutFirst { .. } => None,
                                DownloadScript::CutEvery { bytes } => Some(bytes),
                            };
                            if start == 0 {
                                let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                                let _ = stream.write_all(header.as_bytes());
                            } else {
                                let end = body.len().saturating_sub(1);
                                let header = format!(
                                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    body.len(),
                                    slice.len()
                                );
                                let _ = stream.write_all(header.as_bytes());
                            }
                            let send = cut.map(|n| n.min(slice.len())).unwrap_or(slice.len());
                            let _ = stream.write_all(&slice[..send]);
                            drop(stream);
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(5)),
                        Err(_) => break,
                    }
                }
            });
            Self {
                url: format!("http://127.0.0.1:{port}/Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf"),
                requests,
                stop,
                join: Some(join),
            }
        }
    }

    impl Drop for TestDownload {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.join.take() {
                let _ = handle.join();
            }
        }
    }

    fn header_range_start(req: &str) -> Option<u64> {
        for line in req.lines() {
            let lower = line.to_ascii_lowercase();
            let Some(rest) = lower.strip_prefix("range:") else { continue };
            let spec = rest.trim().strip_prefix("bytes=")?;
            return spec.split('-').next()?.trim().parse().ok();
        }
        None
    }

    fn slow_command() -> Command {
        if cfg!(windows) {
            let mut cmd = Command::new("ping");
            cmd.args(["-n", "40", "127.0.0.1"]);
            cmd
        } else {
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            cmd
        }
    }

    fn echo_command() -> Command {
        if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", "echo ok"]);
            cmd
        } else {
            let mut cmd = Command::new("echo");
            cmd.arg("ok");
            cmd
        }
    }

    fn server_spec(program: PathBuf, model: PathBuf, backend: GpuBackend, log: &Path) -> ServerSpec {
        ServerSpec {
            program,
            model,
            backend,
            help: String::new(),
            free_mib: None,
            fell_back: backend == GpuBackend::Cpu,
            extra_env: vec![("FAUXRRT_LAUNCH_LOG".into(), log.display().to_string())],
        }
    }

    fn write_exe(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    const FAKE_LLAMA_SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

args = sys.argv[1:]
port = int(args[args.index("--port") + 1])
log = os.environ.get("FAUXRRT_LAUNCH_LOG")
if log:
    with open(log, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(args) + "\n")
classification = {
    "header_lines": 1,
    "delimiter": ",",
    "frames": {"position": "LLA"},
    "units": {"position": "m"},
    "columns": {"col_0": "time", "col_1": "pos_lat", "col_2": "pos_lon", "col_3": "pos_alt"},
    "confidence_score": 0.9,
    "unsupported_flag": False,
    "reasoning": "lat lon alt",
}

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        self._send(b'{"status":"ok"}')

    def do_POST(self):
        size = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(size)
        req_log = os.environ.get("FAUXRRT_REQUEST_LOG")
        if req_log:
            with open(req_log, "ab") as fh:
                fh.write(raw + b"\n")
        body = json.dumps({"content": json.dumps(classification)}).encode()
        self._send(body)

    def _send(self, body):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        return

ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#;

    const CRASH_LLAMA_SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys
log = os.environ.get("FAUXRRT_LAUNCH_LOG")
if log:
    with open(log, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(sys.argv[1:]) + "\n")
sys.stderr.write("ggml_cuda_init: failed to initialize CUDA\n")
sys.exit(1)
"#;

    #[cfg(unix)]
    #[test]
    fn second_classification_does_not_relaunch_the_server() {
        let dir = std::env::temp_dir().join(format!("fauxrrt-reuse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let program = write_exe(&dir, "llama-server", FAKE_LLAMA_SERVER);
        let log = dir.join("launches.jsonl");
        let requests = dir.join("requests.jsonl");
        let model = dir.join("model.gguf");
        let mut spec = server_spec(program, model, GpuBackend::Cuda, &log);
        spec.extra_env.push(("FAUXRRT_REQUEST_LOG".into(), requests.display().to_string()));
        spec.fell_back = false;
        let pool = Arc::new(LlamaServerPool::new());
        let cancel = Arc::new(AtomicBool::new(false));
        let info = pool.start_or_fallback(&spec, || Ok(spec.clone()), &cancel).unwrap();
        assert_eq!(info.backend, GpuBackend::Cuda);
        assert_eq!(info.layers, 32);
        assert!(!info.fell_back);
        assert_eq!(pool.launch_count(), 1);
        let runtime = LlamaRuntime::from_info(Arc::clone(&pool), info, Arc::clone(&cancel));
        assert_eq!(runtime.backend, GpuBackend::Cuda);
        assert_eq!(runtime.layers, 32);
        assert_eq!(runtime.status_device(), "CUDA, 32 layers");
        let excerpt = "time,lat,lon,alt\n0,32.4,-106.4,1000\n";
        let first = classify_with(&runtime, excerpt).unwrap();
        let second = classify_with(&runtime, excerpt).unwrap();
        assert_eq!(first.frames.position, "LLA");
        assert_eq!(second.frames.position, "LLA");
        assert_eq!(pool.launch_count(), 1, "a second classification relaunched llama-server");
        let argv = fs::read_to_string(&log).unwrap();
        assert_eq!(argv.lines().count(), 1, "{argv}");
        assert!(argv.contains("\"-ngl\""), "{argv}");
        assert!(argv.contains("\"32\""), "{argv}");
        assert!(argv.contains("\"-c\""), "{argv}");
        assert!(argv.contains("\"2048\""), "{argv}");
        assert!(argv.contains("\"--fit\""), "{argv}");
        assert!(argv.contains("\"off\""), "{argv}");
        let posted = fs::read_to_string(&requests).unwrap();
        assert_eq!(posted.lines().count(), 2, "{posted}");
        assert!(posted.contains("time,lat,lon,alt"));
        assert!(posted.contains("root ::="), "grammar was not sent: {posted}");
        assert!(posted.contains("\"n_predict\":384") || posted.contains("\"n_predict\": 384"), "{posted}");
        pool.shutdown();
        assert!(pool.current().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn cpu_server_starts_only_after_the_gpu_backend_exits() {
        let dir = std::env::temp_dir().join(format!("fauxrrt-fallback-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let crash = write_exe(&dir, "cuda-server", CRASH_LLAMA_SERVER);
        let cpu = write_exe(&dir, "cpu-server", FAKE_LLAMA_SERVER);
        let log = dir.join("launches.jsonl");
        let model = dir.join("model.gguf");
        let preferred = server_spec(crash, model.clone(), GpuBackend::Cuda, &log);
        let cpu_spec = server_spec(cpu, model, GpuBackend::Cpu, &log);
        let pool = Arc::new(LlamaServerPool::new());
        let cancel = AtomicBool::new(false);
        let info = pool
            .start_or_fallback(&preferred, || Ok(cpu_spec.clone()), &cancel)
            .unwrap();
        assert!(info.fell_back);
        assert_eq!(info.backend, GpuBackend::Cpu);
        assert_eq!(info.layers, 0);
        assert_eq!(pool.launch_count(), 2);
        let again = pool
            .start_or_fallback(&preferred, || Ok(cpu_spec.clone()), &cancel)
            .unwrap();
        assert_eq!(again.backend, GpuBackend::Cpu);
        assert_eq!(pool.launch_count(), 2, "a healthy server was relaunched");
        let lines: Vec<_> = fs::read_to_string(&log).unwrap().lines().map(str::to_string).collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("\"32\""), "GPU attempt was not 32 layers: {}", lines[0]);
        assert!(lines[1].contains("\"-ngl\"") && lines[1].contains("\"0\""), "{}", lines[1]);
        pool.shutdown();
        let _ = fs::remove_dir_all(&dir);
    }
}
