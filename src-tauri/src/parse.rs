use std::fs;
use std::path::Path;

use crate::geodesy::ecef_to_lla;
use crate::schema::{
    detect_preview, detect_schema, parse_number, parse_time_value, row_is_header, ColumnMapping,
    DetectedSchema, Frame,
};

#[derive(Debug, Clone)]
pub struct ParsedTrack {
    pub schema: DetectedSchema,
    pub times: Option<Vec<f64>>,
    pub lla: Vec<f32>,
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
    Ok(ParsedTrack { schema, times, lla })
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
