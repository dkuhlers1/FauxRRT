use std::fs;
use std::path::Path;

use crate::classify::{
    column_roles, delimiter_of, origin_from_text, position_indexes, scale_accel, scale_angle_to_rad, scale_length,
    scale_mass, scale_speed, TrajectoryClassification,
};
use crate::geodesy::{ecef_to_lla, eci_to_ecef, enu_to_lla, gmst_rad};
use crate::schema::{
    detect_preview, detect_schema, parse_number, parse_time_value, row_is_header, ColumnMapping,
    DetectedSchema, Frame,
};

#[derive(Debug, Clone, Default)]
pub struct LoadedStates {
    pub velocity_mps: Option<Vec<f32>>,
    pub acceleration_mps2: Option<Vec<f32>>,
    pub orientation_rad: Option<Vec<f32>>,
    pub mass_kg: Option<Vec<f32>>,
}

#[derive(Debug, Clone)]
pub struct ParsedTrack {
    pub schema: DetectedSchema,
    pub times: Option<Vec<f64>>,
    pub lla: Vec<f32>,
    pub states: LoadedStates,
    pub classification: Option<TrajectoryClassification>,
    pub origin_lat: Option<f64>,
    pub origin_lon: Option<f64>,
    pub origin_alt_m: Option<f64>,
}

impl ParsedTrack {
    pub fn from_lla(schema: DetectedSchema, times: Option<Vec<f64>>, lla: Vec<f32>) -> Self {
        Self {
            schema,
            times,
            lla,
            states: LoadedStates::default(),
            classification: None,
            origin_lat: None,
            origin_lon: None,
            origin_alt_m: None,
        }
    }
}

pub fn read_trajectory_text(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(decode_text(&bytes))
}

pub fn parse_path_with_classification(
    path: &Path,
    classification: &TrajectoryClassification,
    origin: Option<(f64, f64, f64)>,
) -> Result<ParsedTrack, String> {
    parse_with_classification(&read_trajectory_text(path)?, classification, origin)
}

pub fn parse_path(path: &Path) -> Result<ParsedTrack, String> {
    parse_path_with(path, None)
}

pub fn parse_path_with(path: &Path, mapping: Option<&ColumnMapping>) -> Result<ParsedTrack, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let text = decode_text(&bytes);
    parse_text(&text, mapping)
}

pub fn parse_text(text: &str, mapping: Option<&ColumnMapping>) -> Result<ParsedTrack, String> {
    let preview = detect_preview(text).ok_or_else(|| "could not detect a columnar schema".to_string())?;
    let mut schema = detect_schema(&preview);
    if let Some(mapping) = mapping {
        schema.apply_mapping(mapping);
    }
    if !schema.is_usable() {
        return Err("could not find lat/lon or ECEF X/Y/Z columns".into());
    }
    parse_with_schema(text, schema)
}

pub fn parse_with_schema(text: &str, schema: DetectedSchema) -> Result<ParsedTrack, String> {
    let delim = match schema.delimiter.as_str() {
        "tab" => crate::schema::Delimiter::Tab,
        "semicolon" => crate::schema::Delimiter::Semicolon,
        "pipe" => crate::schema::Delimiter::Pipe,
        "whitespace" => crate::schema::Delimiter::Whitespace,
        _ => crate::schema::Delimiter::Comma,
    };

    let mut times = Vec::new();
    let mut lla = Vec::new();

    for line in text.lines() {
        if crate::schema::is_comment_line(line) {
            continue;
        }
        let fields = delim.split(line.trim_end());
        if fields.len() < 2 {
            continue;
        }
        if schema.has_header && row_is_header(&fields) {
            continue;
        }
        match schema.frame {
            Frame::Lla => {
                let lat = schema
                    .lat_col
                    .and_then(|i| fields.get(i))
                    .and_then(|v| parse_number(v));
                let lon = schema
                    .lon_col
                    .and_then(|i| fields.get(i))
                    .and_then(|v| parse_number(v));
                let Some(lat) = lat else { continue };
                let Some(lon) = lon else { continue };
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    continue;
                }
                let alt = schema
                    .alt_col
                    .and_then(|i| fields.get(i))
                    .and_then(|v| parse_number(v))
                    .map(|v| if schema_alt_is_km(&schema) { v * 1000.0 } else { v })
                    .unwrap_or(0.0);
                lla.push(lon as f32);
                lla.push(lat as f32);
                lla.push(alt as f32);
            }
            Frame::Ecef => {
                let x = schema.x_col.and_then(|i| fields.get(i)).and_then(|v| parse_number(v));
                let y = schema.y_col.and_then(|i| fields.get(i)).and_then(|v| parse_number(v));
                let z = schema.z_col.and_then(|i| fields.get(i)).and_then(|v| parse_number(v));
                let (Some(mut x), Some(mut y), Some(mut z)) = (x, y, z) else { continue };
                if schema_xyz_is_km(&schema) {
                    x *= 1000.0;
                    y *= 1000.0;
                    z *= 1000.0;
                }
                let (lat, lon, alt) = ecef_to_lla(x, y, z);
                lla.push(lon as f32);
                lla.push(lat as f32);
                lla.push(alt as f32);
            }
        }
        if let Some(i) = schema.time_col {
            if let Some(raw) = fields.get(i) {
                times.push(parse_time_value(raw).unwrap_or(times.last().copied().unwrap_or(0.0)));
            }
        }
    }

    let n = lla.len() / 3;
    if n < 2 {
        return Err("file did not contain at least two valid trajectory states".into());
    }
    let times = if times.len() == n { Some(times) } else { None };
    Ok(ParsedTrack::from_lla(schema, times, lla))
}

pub fn parse_with_classification(
    text: &str,
    classification: &TrajectoryClassification,
    origin: Option<(f64, f64, f64)>,
) -> Result<ParsedTrack, String> {
    let frame = crate::classify::canonical_frame(&classification.coordinate_system)
        .ok_or_else(|| format!("coordinate system {} is not supported", classification.coordinate_system))?;
    let delim = delimiter_of(&classification.delimiter)?;
    let (_time_col, a_col, b_col, c_col) = position_indexes(classification)?;
    let roles = column_roles(classification);
    let length_scale = scale_length(&classification.units.position)?;
    let speed_scale = classification.units.velocity.as_deref().map(scale_speed).transpose()?;
    let accel_scale = classification.units.acceleration.as_deref().map(scale_accel).transpose()?;
    let angle_scale = classification.units.orientation.as_deref().map(scale_angle_to_rad).transpose()?;
    let mass_scale = classification.units.mass.as_deref().map(scale_mass).transpose()?;
    let origin = origin.or_else(|| origin_from_text(text, classification.header_lines));
    if matches!(frame, "NED" | "NEU") && origin.is_none() {
        return Err("NED and NEU need an origin latitude and longitude".into());
    }

    let mut times = Vec::new();
    let mut lla = Vec::new();
    let mut velocity = Vec::new();
    let mut acceleration = Vec::new();
    let mut orientation = Vec::new();
    let mut mass = Vec::new();
    let mut raw_times = Vec::new();

    for (line_index, line) in text.lines().enumerate() {
        if line_index < classification.header_lines as usize || line.trim().is_empty() {
            continue;
        }
        let fields = delim.split(line.trim_end());
        if fields.len() < 2 {
            continue;
        }
        let time = roles
            .iter()
            .find(|(_, role)| role.as_str() == "time")
            .and_then(|(index, _)| fields.get(*index))
            .and_then(|raw| parse_time_value(raw));
        let Some(point) = to_internal_frame(frame, &fields, a_col, b_col, c_col, length_scale, origin) else {
            continue;
        };
        if let Some(scale) = speed_scale {
            if let Some(vec) = vector_at(&fields, &roles, "vel_", frame, scale) {
                velocity.extend(vec);
            }
        }
        if let Some(scale) = accel_scale {
            if let Some(vec) = vector_at(&fields, &roles, "acc_", frame, scale) {
                acceleration.extend(vec);
            }
        }
        if let Some(scale) = angle_scale {
            if let Some(vec) = orientation_at(&fields, &roles, scale) {
                orientation.extend(vec);
            }
        }
        if let Some(scale) = mass_scale {
            if let Some(index) = roles.iter().find(|(_, role)| role.as_str() == "mass").map(|(i, _)| *i) {
                if let Some(value) = number_at(&fields, index) {
                    mass.push((value * scale) as f32);
                }
            }
        }
        lla.push(point.0 as f32);
        lla.push(point.1 as f32);
        lla.push(point.2 as f32);
        if frame == "ECI" {
            raw_times.push(time.unwrap_or(0.0));
        }
        if let Some(time) = time {
            times.push(time);
        }
    }

    if frame == "ECI" {
        apply_eci_rotation(&mut lla, &raw_times);
    }

    let n = lla.len() / 3;
    if n < 2 {
        return Err("file did not contain at least two valid trajectory states".into());
    }
    let states = LoadedStates {
        velocity_mps: full_channel(velocity, n, 3),
        acceleration_mps2: full_channel(acceleration, n, 3),
        orientation_rad: full_channel(orientation, n, 3),
        mass_kg: full_channel(mass, n, 1),
    };
    let (origin_lat, origin_lon, origin_alt_m) = match origin {
        Some((lat, lon, alt)) => (Some(lat), Some(lon), Some(alt)),
        None => (None, None, None),
    };
    Ok(ParsedTrack {
        schema: DetectedSchema::generated(),
        times: if times.len() == n { Some(times) } else { None },
        lla,
        states,
        classification: Some(classification.clone()),
        origin_lat,
        origin_lon,
        origin_alt_m,
    })
}

/// Convert one position from the coordinate system Llama identified into the internal
/// geodetic frame: longitude degrees, latitude degrees, altitude metres.
/// The map and the risk calculation both read this frame. The model does not convert.
pub(crate) fn to_internal_frame(
    frame: &str,
    fields: &[String],
    a_col: usize,
    b_col: usize,
    c_col: usize,
    length_scale: f64,
    origin: Option<(f64, f64, f64)>,
) -> Option<(f64, f64, f64)> {
    match frame {
        "LLA" => {
            let lat = number_at(fields, a_col)?;
            let lon = number_at(fields, b_col)?;
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return None;
            }
            let alt = if c_col == usize::MAX {
                0.0
            } else {
                number_at(fields, c_col).unwrap_or(0.0) * length_scale
            };
            Some((lon, lat, alt))
        }
        "ECEF" | "ECI" => {
            let (x, y, z) = scaled_xyz(fields, a_col, b_col, c_col, length_scale)?;
            let (lon, lat, alt) = ecef_to_lla(x, y, z);
            Some((lon, lat, alt))
        }
        "NED" => {
            let (lat0, lon0, alt0) = origin?;
            let north = number_at(fields, a_col)? * length_scale;
            let east = number_at(fields, b_col)? * length_scale;
            let down = number_at(fields, c_col)? * length_scale;
            Some(enu_to_lla(lat0, lon0, alt0, east, north, -down))
        }
        "NEU" => {
            let (lat0, lon0, alt0) = origin?;
            let north = number_at(fields, a_col)? * length_scale;
            let east = number_at(fields, b_col)? * length_scale;
            let up = number_at(fields, c_col)? * length_scale;
            Some(enu_to_lla(lat0, lon0, alt0, east, north, up))
        }
        _ => None,
    }
}

fn number_at(fields: &[String], index: usize) -> Option<f64> {
    fields.get(index).and_then(|value| parse_number(value))
}

fn scaled_xyz(fields: &[String], x: usize, y: usize, z: usize, scale: f64) -> Option<(f64, f64, f64)> {
    Some((
        number_at(fields, x)? * scale,
        number_at(fields, y)? * scale,
        number_at(fields, z)? * scale,
    ))
}

fn vector_at(fields: &[String], roles: &std::collections::BTreeMap<usize, String>, prefix: &str, frame: &str, scale: f64) -> Option<[f32; 3]> {
    let names: [&str; 3] = match frame {
        "LLA" => ["n", "e", "u"],
        "NED" => ["n", "e", "d"],
        "NEU" => ["n", "e", "u"],
        _ => ["x", "y", "z"],
    };
    let mut out = [0.0; 3];
    for (slot, suffix) in names.iter().enumerate() {
        let role = format!("{prefix}{suffix}");
        let alt = match (*suffix, frame) {
            ("n", _) => format!("{prefix}x"),
            ("e", _) => format!("{prefix}y"),
            ("d", _) | ("u", _) => format!("{prefix}z"),
            _ => role.clone(),
        };
        let index = roles
            .iter()
            .find(|(_, name)| name.as_str() == role || name.as_str() == alt)
            .map(|(index, _)| *index)?;
        out[slot] = (number_at(fields, index)? * scale) as f32;
    }
    Some(out)
}

fn orientation_at(fields: &[String], roles: &std::collections::BTreeMap<usize, String>, scale: f64) -> Option<[f32; 3]> {
    let mut out = [0.0; 3];
    for (slot, name) in ["orientation_roll", "orientation_pitch", "orientation_yaw"].iter().enumerate() {
        let index = roles.iter().find(|(_, role)| role.as_str() == *name).map(|(index, _)| *index)?;
        out[slot] = (number_at(fields, index)? * scale) as f32;
    }
    Some(out)
}

fn full_channel(values: Vec<f32>, points: usize, width: usize) -> Option<Vec<f32>> {
    if values.len() == points * width {
        Some(values)
    } else {
        None
    }
}

fn apply_eci_rotation(lla: &mut [f32], times: &[f64]) {
    if lla.len() / 3 != times.len() || times.is_empty() {
        return;
    }
    let elapsed = times.iter().all(|t| t.abs() < 1.0e8);
    let theta0 = if elapsed { 0.0 } else { gmst_rad(times[0]) };
    let t0 = times[0];
    for (i, time) in times.iter().enumerate() {
        let lon = lla[i * 3] as f64;
        let lat = lla[i * 3 + 1] as f64;
        let alt = lla[i * 3 + 2] as f64;
        let (x, y, z) = crate::geodesy::lla_to_ecef(lat, lon, alt);
        let theta = if elapsed {
            7.2921150e-5 * (time - t0)
        } else {
            gmst_rad(*time) - theta0
        };
        let (x, y, z) = eci_to_ecef(x, y, z, theta);
        let (lon, lat, alt) = ecef_to_lla(x, y, z);
        lla[i * 3] = lon as f32;
        lla[i * 3 + 1] = lat as f32;
        lla[i * 3 + 2] = alt as f32;
    }
}

fn decode_text(bytes: &[u8]) -> String {
    let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };
    String::from_utf8_lossy(bytes).into_owned()
}

fn schema_alt_is_km(schema: &DetectedSchema) -> bool {
    schema
        .alt_col
        .and_then(|i| schema.columns.get(i))
        .map(|c| c.name.to_ascii_lowercase().contains("km"))
        .unwrap_or(false)
}

fn schema_xyz_is_km(schema: &DetectedSchema) -> bool {
    schema
        .x_col
        .and_then(|i| schema.columns.get(i))
        .map(|c| c.name.to_ascii_lowercase().contains("km"))
        .unwrap_or(false)
}

pub fn downsample_lla(lla: &[f32], budget: usize) -> Vec<f32> {
    let n = lla.len() / 3;
    if n <= budget || budget < 3 {
        return lla.to_vec();
    }
    lttb(lla, budget)
}

fn lttb(lla: &[f32], threshold: usize) -> Vec<f32> {
    let n = lla.len() / 3;
    let mut out = Vec::with_capacity(threshold * 3);
    push_point(&mut out, lla, 0);

    let bucket_size = (n - 2) as f64 / (threshold - 2) as f64;
    let mut prev = 0usize;

    for i in 0..(threshold - 2) {
        let start = ((i as f64 + 1.0) * bucket_size).floor() as usize + 1;
        let end = (((i as f64 + 2.0) * bucket_size).floor() as usize + 1).min(n);
        let next_start = end;
        let next_end = ((((i as f64 + 3.0) * bucket_size).floor() as usize) + 1).min(n);
        let (ax, ay) = xy(lla, prev);
        let (mut mx, mut my, mut mc) = (0.0, 0.0, 0.0);
        for j in next_start..next_end.max(next_start + 1).min(n) {
            let (x, y) = xy(lla, j);
            mx += x;
            my += y;
            mc += 1.0;
        }
        if mc > 0.0 {
            mx /= mc;
            my /= mc;
        } else {
            let (x, y) = xy(lla, (next_start).min(n - 1));
            mx = x;
            my = y;
        }

        let mut best_i = start;
        let mut best_area = -1.0;
        for j in start..end.max(start + 1).min(n) {
            let (bx, by) = xy(lla, j);
            let area = ((ax - mx) * (by - ay) - (ax - bx) * (my - ay)).abs();
            if area > best_area {
                best_area = area;
                best_i = j;
            }
        }
        push_point(&mut out, lla, best_i);
        prev = best_i;
    }

    push_point(&mut out, lla, n - 1);
    out
}

fn xy(lla: &[f32], i: usize) -> (f64, f64) {
    (lla[i * 3] as f64, lla[i * 3 + 1] as f64)
}

fn push_point(out: &mut Vec<f32>, lla: &[f32], i: usize) {
    let o = i * 3;
    out.extend_from_slice(&lla[o..o + 3]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::classify::{
        classify_with, may_load, ClassificationUnits, TextCompleter, TrajectoryClassification,
    };

    struct Stub(String);

    impl TextCompleter for Stub {
        fn complete(&self, _system: &str, _user: &str) -> Result<String, String> {
            Ok(self.0.clone())
        }
    }

    fn classified(raw: TrajectoryClassification) -> TrajectoryClassification {
        let json = serde_json::to_string(&raw).unwrap();
        classify_with(&Stub(json), "fixture excerpt").unwrap()
    }

    fn base(frame: &str, position: &str, columns: &[(&str, &str)]) -> TrajectoryClassification {
        let mut map = BTreeMap::new();
        for (key, role) in columns {
            map.insert((*key).to_string(), (*role).to_string());
        }
        TrajectoryClassification {
            header_lines: 1,
            delimiter: ",".into(),
            coordinate_system: frame.into(),
            units: ClassificationUnits {
                position: position.into(),
                velocity: Some("m/s".into()),
                acceleration: Some("m/s^2".into()),
                orientation: Some("rad".into()),
                mass: None,
            },
            columns: map,
            confidence_score: 0.92,
            unsupported_flag: false,
            reasoning: "fixture".into(),
        }
    }

    fn load_detected(text: &str, class: &TrajectoryClassification) -> Result<ParsedTrack, String> {
        if !may_load(class, false) {
            return Err("manual assignment required".into());
        }
        parse_with_classification(text, class, None)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-2
    }

    #[test]
    fn stub_classifier_loads_mixed_column_order() {
        let ordered = classified(base(
            "ECEF",
            "m",
            &[("col_0", "time"), ("col_1", "pos_x"), ("col_2", "pos_y"), ("col_3", "pos_z")],
        ));
        let mixed = classified(base(
            "ECEF",
            "m",
            &[("col_0", "time"), ("col_1", "pos_z"), ("col_2", "pos_x"), ("col_3", "pos_y")],
        ));
        let standard = "t,x,y,z\n0,6378137,0,0\n1,6378137,0,1000\n";
        let swapped = "t,z,x,y\n0,0,6378137,0\n1,1000,6378137,0\n";
        let a = load_detected(standard, &ordered).unwrap();
        let b = load_detected(swapped, &mixed).unwrap();
        assert_eq!(a.lla.len(), b.lla.len());
        for (left, right) in a.lla.iter().zip(b.lla.iter()) {
            assert!((left - right).abs() < 1e-2, "{left} vs {right}");
        }
        assert!(close(a.lla[1] as f64, 0.0));
    }

    #[test]
    fn stub_classifier_loads_si_and_imperial_units() {
        let mut imperial = base(
            "LLA",
            "ft",
            &[
                ("col_0", "time"),
                ("col_1", "pos_lat"),
                ("col_2", "pos_lon"),
                ("col_3", "pos_alt"),
                ("col_4", "vel_n"),
                ("col_5", "vel_e"),
                ("col_6", "vel_u"),
                ("col_7", "mass"),
            ],
        );
        imperial.units.velocity = Some("ft/s".into());
        imperial.units.mass = Some("lbm".into());
        imperial.units.orientation = Some("deg".into());
        imperial.columns.insert("col_8".into(), "orientation_roll".into());
        imperial.columns.insert("col_9".into(), "orientation_pitch".into());
        imperial.columns.insert("col_10".into(), "orientation_yaw".into());
        let class = classified(imperial);
        let text = "\
t,lat,lon,alt,vn,ve,vu,m,r,p,y
0,32.4,-106.4,1000,10,0,0,2,180,90,0
1,32.5,-106.3,2000,10,0,0,2,180,90,0
";
        let track = load_detected(text, &class).unwrap();
        assert!(close(track.lla[2] as f64, 304.8));
        let velocity = track.states.velocity_mps.expect("velocity");
        assert!(close(velocity[0] as f64, 3.048));
        let mass = track.states.mass_kg.expect("mass");
        assert!(close(mass[0] as f64, 0.90718474));
        let attitude = track.states.orientation_rad.expect("orientation");
        assert!(close(attitude[0] as f64, std::f64::consts::PI));
    }

    #[test]
    fn stub_classifier_loads_each_supported_frame() {
        let lla = classified(base(
            "LLA",
            "m",
            &[("col_0", "time"), ("col_1", "pos_lat"), ("col_2", "pos_lon"), ("col_3", "pos_alt")],
        ));
        let lla_track = load_detected("t,lat,lon,alt\n0,32.4,-106.4,1000\n1,32.5,-106.3,1100\n", &lla).unwrap();
        assert!(close(lla_track.lla[1] as f64, 32.4));
        assert!(close(lla_track.lla[0] as f64, -106.4));

        let ecef = classified(base(
            "ECEF",
            "m",
            &[("col_0", "time"), ("col_1", "pos_x"), ("col_2", "pos_y"), ("col_3", "pos_z")],
        ));
        let ecef_track = load_detected("t,x,y,z\n0,6378137,0,0\n1,6378137,0,1000\n", &ecef).unwrap();
        assert!(close(ecef_track.lla[0] as f64, 0.0));
        assert!(ecef_track.lla[2].abs() < 1.0);

        let mut ned = base(
            "NED",
            "m",
            &[("col_0", "time"), ("col_1", "pos_n"), ("col_2", "pos_e"), ("col_3", "pos_d")],
        );
        ned.header_lines = 3;
        let ned = classified(ned);
        let ned_text = "\
latitude: 32.4
longitude: -106.4
elevation: 1200
0,0,0,0
1,1000,0,0
";
        let ned_track = load_detected(ned_text, &ned).unwrap();
        assert!(close(ned_track.lla[1] as f64, 32.4));
        assert!(ned_track.lla[4] > ned_track.lla[1]);

        let mut neu = base(
            "NEU",
            "m",
            &[("col_0", "time"), ("col_1", "pos_n"), ("col_2", "pos_e"), ("col_3", "pos_u")],
        );
        neu.header_lines = 3;
        let neu = classified(neu);
        let neu_text = "\
latitude: 10
longitude: 20
elevation: 100
0,0,0,0
1,0,0,50
";
        let neu_track = load_detected(neu_text, &neu).unwrap();
        assert!(neu_track.lla[5] > neu_track.lla[2] + 40.0);

        let eci = classified(base(
            "ECI",
            "m",
            &[("col_0", "time"), ("col_1", "pos_x"), ("col_2", "pos_y"), ("col_3", "pos_z")],
        ));
        let eci_track = load_detected("t,x,y,z\n0,6378137,0,0\n2000,6378137,0,0\n", &eci).unwrap();
        assert!((eci_track.lla[3] - eci_track.lla[0]).abs() > 1.0);
    }

    #[test]
    fn unsupported_frame_asks_for_manual_assignment() {
        let mut raw = base(
            "BODY",
            "m",
            &[("col_0", "time"), ("col_1", "pos_x"), ("col_2", "pos_y"), ("col_3", "pos_z")],
        );
        raw.confidence_score = 0.95;
        let class = classified(raw);
        assert!(class.unsupported_flag);
        assert!(!may_load(&class, false));
        assert!(!may_load(&class, true));
        let err = load_detected("t,x,y,z\n0,1,2,3\n1,1,2,4\n", &class).unwrap_err();
        assert!(err.contains("manual"));
        assert!(parse_with_classification("t,x,y,z\n0,1,2,3\n1,1,2,4\n", &class, None).is_err());
    }

    #[test]
    fn low_confidence_asks_for_manual_assignment() {
        let mut raw = base(
            "ECEF",
            "m",
            &[("col_0", "time"), ("col_1", "pos_x"), ("col_2", "pos_y"), ("col_3", "pos_z")],
        );
        raw.confidence_score = 0.2;
        let class = classified(raw);
        assert!(!may_load(&class, false));
        let err = load_detected("t,x,y,z\n0,6378137,0,0\n1,6378137,0,1000\n", &class).unwrap_err();
        assert!(err.contains("manual"));
        assert!(may_load(&class, true));
        let track = parse_with_classification(
            "t,x,y,z\n0,6378137,0,0\n1,6378137,0,1000\n",
            &class,
            None,
        )
        .unwrap();
        assert!(track.lla.len() >= 6);
    }

    #[test]
    fn eglin_keywest_csv_is_lla_with_position_only() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("samples")
            .join("eglin_keywest_6dof.csv");
        let text = std::fs::read_to_string(&path).unwrap();
        let heuristic = parse_text(&text, None).unwrap();
        assert!(heuristic.lla.len() / 3 >= 300, "heuristic points {}", heuristic.lla.len() / 3);
        assert!(close(heuristic.lla[0] as f64, -86.5254));
        assert!(close(heuristic.lla[1] as f64, 30.4832));
        assert!(heuristic.states.velocity_mps.is_none());
        assert!(heuristic.states.acceleration_mps2.is_none());
        assert!(heuristic.states.orientation_rad.is_none());
        assert!(heuristic.states.mass_kg.is_none());

        let class = classified(base(
            "LLA",
            "m",
            &[
                ("col_0", "time"),
                ("col_1", "pos_lat"),
                ("col_2", "pos_lon"),
                ("col_3", "pos_alt"),
            ],
        ));
        assert!(may_load(&class, false));
        let track = load_detected(&text, &class).unwrap();
        assert_eq!(track.lla.len(), heuristic.lla.len());
        assert!(close(track.lla[1] as f64, 30.4832));
        assert!(track.states.velocity_mps.is_none());
        assert!(track.states.mass_kg.is_none());
    }

    #[test]
    fn to_internal_frame_converts_lla_ecr_and_ned() {
        let (x, y, z) = crate::geodesy::lla_to_ecef(30.4832, -86.5254, 26.0);
        let (x2, y2, z2) = crate::geodesy::lla_to_ecef(30.4900, -86.5100, 120.0);
        let ecr = format!("time,x-ecr,z-ecr,y-ecr\n0,{x},{z},{y}\n1,{x2},{z2},{y2}\n");
        let ecr_class = classified(base(
            "ECEF",
            "m",
            &[
                ("col_0", "time"),
                ("col_1", "pos_x"),
                ("col_2", "pos_z"),
                ("col_3", "pos_y"),
            ],
        ));
        let fields = ecr.lines().nth(1).unwrap().split(',').map(|s| s.to_string()).collect::<Vec<_>>();
        let one = to_internal_frame("ECEF", &fields, 1, 3, 2, 1.0, None).unwrap();
        assert!(close(one.0, -86.5254));
        assert!(close(one.1, 30.4832));
        assert!((one.2 - 26.0).abs() < 1.0);
        let track = load_detected(&ecr, &ecr_class).unwrap();
        assert!(close(track.lla[0] as f64, -86.5254));
        assert!(close(track.lla[1] as f64, 30.4832));
        assert!(close(track.lla[3] as f64, -86.5100));
        assert!(close(track.lla[4] as f64, 30.4900));

        let ned = "time,north,east,down\n0,0,0,0\n1,1000,200,-50\n";
        let ned_class = classified(base(
            "NED",
            "m",
            &[
                ("col_0", "time"),
                ("col_1", "pos_n"),
                ("col_2", "pos_e"),
                ("col_3", "pos_d"),
            ],
        ));
        let origin = (30.4832, -86.5254, 26.0);
        let track = parse_with_classification(ned, &ned_class, Some(origin)).unwrap();
        let (lon, lat, alt) = crate::geodesy::enu_to_lla(30.4832, -86.5254, 26.0, 200.0, 1000.0, 50.0);
        assert!(close(track.lla[0] as f64, -86.5254));
        assert!(close(track.lla[1] as f64, 30.4832));
        assert!(close(track.lla[3] as f64, lon));
        assert!(close(track.lla[4] as f64, lat));
        assert!((track.lla[5] as f64 - alt).abs() < 1.0);
    }
}
