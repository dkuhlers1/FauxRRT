//! Boat reports loaded from KML placemarks (AIS-style receptors).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::kde::{sample_density, KdeGrid};
use serde::{Deserialize, Serialize};

const PREDICT_HORIZON_S: f64 = 3600.0;
const PREDICT_STEPS: usize = 5;
const METERS_PER_NM: f64 = 1852.0;
const EARTH_M: f64 = 6_371_000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Boat {
    pub id: u64,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    #[serde(default)]
    pub alt_m: f64,
    #[serde(default)]
    pub speed_kn: Option<f64>,
    #[serde(default)]
    pub heading_deg: Option<f64>,
    #[serde(default)]
    pub people_on_board: Option<u32>,
    #[serde(default)]
    pub length_m: Option<f64>,
    #[serde(default)]
    pub age_s: Option<f64>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub color: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct BoatView {
    pub id: u64,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    pub alt_m: f64,
    pub speed_kn: Option<f64>,
    pub heading_deg: Option<f64>,
    pub people_on_board: Option<u32>,
    pub length_m: Option<f64>,
    pub age_s: Option<f64>,
    pub source: Option<String>,
    pub visible: bool,
    pub color: String,
    pub estimate_lon: f64,
    pub estimate_lat: f64,
    pub uncertainty_m: f64,
    pub predict_lla: Vec<f32>,
    #[serde(default)]
    pub area_m2: Option<f64>,
    #[serde(default)]
    pub kde_density: Option<f64>,
    #[serde(default)]
    pub p_hit: Option<f64>,
    #[serde(default)]
    pub p_hit_mean: Option<f64>,
    #[serde(default)]
    pub expected_casualties: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct BoatDraft {
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    pub alt_m: f64,
    pub speed_kn: Option<f64>,
    pub heading_deg: Option<f64>,
    pub people_on_board: Option<u32>,
    pub length_m: Option<f64>,
    pub age_s: Option<f64>,
}

impl Boat {
    pub fn view(&self) -> BoatView {
        let (estimate_lon, estimate_lat, uncertainty_m, predict_lla) = predict(self);
        BoatView {
            id: self.id,
            name: self.name.clone(),
            lon: self.lon,
            lat: self.lat,
            alt_m: self.alt_m,
            speed_kn: self.speed_kn,
            heading_deg: self.heading_deg,
            people_on_board: self.people_on_board,
            length_m: self.length_m,
            age_s: self.age_s,
            source: self.source.clone(),
            visible: self.visible,
            color: self.color.clone(),
            estimate_lon,
            estimate_lat,
            uncertainty_m,
            predict_lla,
            area_m2: None,
            kde_density: None,
            p_hit: None,
            p_hit_mean: None,
            expected_casualties: None,
        }
    }
}

pub fn score_boat(mut boat: BoatView, grid: &KdeGrid) -> BoatView {
    let mut samples = Vec::new();
    samples.push((boat.lon, boat.lat));
    samples.push((boat.estimate_lon, boat.estimate_lat));
    for chunk in boat.predict_lla.chunks(3) {
        if chunk.len() >= 2 {
            samples.push((chunk[0] as f64, chunk[1] as f64));
        }
    }
    if boat.uncertainty_m > 50.0 {
        for i in 0..8 {
            samples.push(destination(boat.lon, boat.lat, i as f64 * 45.0, boat.uncertainty_m));
        }
    }
    let dens: Vec<f64> = samples
        .iter()
        .map(|(lon, lat)| sample_density(grid, *lon, *lat).max(0.0))
        .collect();
    let peak = dens.iter().copied().fold(0.0_f64, f64::max);
    let mean = if dens.is_empty() {
        0.0
    } else {
        dens.iter().sum::<f64>() / dens.len() as f64
    };
    let area = boat_area_m2(boat.length_m);
    let p_hit = hit_probability(peak, area);
    let p_mean = hit_probability(mean, area);
    boat.area_m2 = Some(area);
    boat.kde_density = Some(peak);
    boat.p_hit = Some(p_hit);
    boat.p_hit_mean = Some(p_mean);
    boat.expected_casualties = boat.people_on_board.map(|n| p_hit * f64::from(n));
    boat
}

fn boat_area_m2(length_m: Option<f64>) -> f64 {
    let length = length_m.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(12.0);
    let beam = (length / 4.0).max(3.0);
    length * beam
}

fn hit_probability(density_per_m2: f64, area_m2: f64) -> f64 {
    let expected = (density_per_m2.max(0.0) * area_m2.max(0.0)).min(50.0);
    1.0 - (-expected).exp()
}

pub fn boat_color(id: u64) -> String {
    let h = 0.47 + (id as f64 * 0.6180339887498949).fract() * 0.08;
    let (r, g, b) = hsl_to_rgb(h, 0.68, 0.56);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let a = s * l.min(1.0 - l);
    let f = |n: f64| {
        let k = (n + h * 12.0) % 12.0;
        let v = l - a * ((k - 3.0).min(9.0 - k)).max(-1.0).min(1.0);
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    (f(0.0), f(8.0), f(4.0))
}

fn predict(boat: &Boat) -> (f64, f64, f64, Vec<f32>) {
    let mut lla = vec![boat.lon as f32, boat.lat as f32, boat.alt_m as f32];
    let (Some(speed_kn), Some(heading)) = (boat.speed_kn, boat.heading_deg) else {
        return (boat.lon, boat.lat, 0.0, lla);
    };
    if !speed_kn.is_finite() || speed_kn < 0.0 || !heading.is_finite() {
        return (boat.lon, boat.lat, 0.0, lla);
    }
    let speed_mps = speed_kn * METERS_PER_NM / 3600.0;
    let age = boat.age_s.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(0.0);
    let (est_lon, est_lat) = destination(boat.lon, boat.lat, heading, speed_mps * age);
    lla.extend([est_lon as f32, est_lat as f32, boat.alt_m as f32]);
    for i in 1..=PREDICT_STEPS {
        let t = PREDICT_HORIZON_S * (i as f64) / (PREDICT_STEPS as f64);
        let (lon, lat) = destination(est_lon, est_lat, heading, speed_mps * t);
        lla.extend([lon as f32, lat as f32, boat.alt_m as f32]);
    }
    (est_lon, est_lat, speed_mps * age, lla)
}

fn destination(lon: f64, lat: f64, heading_deg: f64, dist_m: f64) -> (f64, f64) {
    if dist_m.abs() < 1e-6 {
        return (lon, lat);
    }
    let brng = heading_deg.to_radians();
    let lat1 = lat.to_radians();
    let lon1 = lon.to_radians();
    let ang = dist_m / EARTH_M;
    let lat2 = (lat1.sin() * ang.cos() + lat1.cos() * ang.sin() * brng.cos()).asin();
    let lon2 = lon1
        + (brng.sin() * ang.sin() * lat1.cos()).atan2(ang.cos() - lat1.sin() * lat2.sin());
    let mut lon_deg = lon2.to_degrees();
    if lon_deg > 180.0 {
        lon_deg -= 360.0;
    } else if lon_deg < -180.0 {
        lon_deg += 360.0;
    }
    (lon_deg, lat2.to_degrees())
}

pub fn parse_kml_path(path: &Path) -> Result<Vec<BoatDraft>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_kml(&text)
}

pub fn parse_kml(text: &str) -> Result<Vec<BoatDraft>, String> {
    let blocks = tagged_blocks(text, "placemark");
    if blocks.is_empty() {
        return Err("no Placemark elements in KML".into());
    }
    let mut boats = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        let Some((lon, lat, alt_m)) = last_coordinate(block) else {
            continue;
        };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }
        let fields = extended_fields(block);
        let name = first_tag_text(block, "name")
            .map(|s| decode_xml(&s))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("Boat {}", i + 1));
        boats.push(BoatDraft {
            name,
            lon,
            lat,
            alt_m,
            speed_kn: field_f64(&fields, &["speed_kn", "speed_kts", "sog_kn", "sog", "speed"]),
            heading_deg: field_f64(
                &fields,
                &["heading_deg", "heading", "cog_deg", "cog", "course"],
            ),
            people_on_board: field_u32(
                &fields,
                &["people_on_board", "pob", "people", "souls", "persons"],
            ),
            length_m: field_length_m(&fields),
            age_s: field_age_s(&fields),
        });
    }
    if boats.is_empty() {
        return Err("KML had Placemarks but no usable Point/coordinates".into());
    }
    Ok(boats)
}

fn field_length_m(fields: &HashMap<String, String>) -> Option<f64> {
    if let Some(v) = field_f64(fields, &["length_m", "loa_m", "size_m", "length", "size", "loa"]) {
        return Some(v);
    }
    field_f64(fields, &["length_ft", "loa_ft"]).map(|ft| ft * 0.3048)
}

fn field_age_s(fields: &HashMap<String, String>) -> Option<f64> {
    if let Some(v) = field_f64(
        fields,
        &[
            "age_s",
            "time_since_update_s",
            "last_update_age_s",
            "stale_s",
            "seconds_since_update",
            "age",
        ],
    ) {
        return Some(v);
    }
    field_f64(fields, &["age_min", "minutes_since_update"]).map(|m| m * 60.0)
}

fn field_f64(fields: &HashMap<String, String>, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|k| fields.get(*k))
        .and_then(|v| parse_number(v))
}

fn field_u32(fields: &HashMap<String, String>, keys: &[&str]) -> Option<u32> {
    field_f64(fields, keys).map(|v| v.round().clamp(0.0, u32::MAX as f64) as u32)
}

fn parse_number(raw: &str) -> Option<f64> {
    let trimmed = raw.trim().trim_matches(|c: char| c == '"' || c == '\'');
    let token = trimmed
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';'))
        .find(|s| !s.is_empty())?;
    token.parse().ok()
}

fn last_coordinate(block: &str) -> Option<(f64, f64, f64)> {
    let inner = first_tag_text(block, "coordinates")?;
    let mut last = None;
    for token in inner.split(|c: char| c.is_whitespace() || c == '\n' || c == '\r') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let mut parts = token.split(',');
        let lon: f64 = parts.next()?.trim().parse().ok()?;
        let lat: f64 = parts.next()?.trim().parse().ok()?;
        let alt: f64 = parts.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0.0);
        last = Some((lon, lat, alt));
    }
    last
}

fn extended_fields(block: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for data in tagged_blocks(block, "data") {
        if let Some(name) = open_tag_attr(block, "data", tagged_block_start(block, "data", &data)) {
            let value = first_tag_text(&data, "value").unwrap_or_else(|| text_content(&data));
            insert_field(&mut fields, &name, &value);
        }
    }
    for data in tagged_blocks(block, "simpledata") {
        if let Some(name) = open_tag_attr(block, "simpledata", tagged_block_start(block, "simpledata", &data))
        {
            insert_field(&mut fields, &name, &text_content(&data));
        }
    }
    if fields.is_empty() {
        if let Some(desc) = first_tag_text(block, "description") {
            parse_description_fields(&desc, &mut fields);
        }
    }
    fields
}

fn insert_field(fields: &mut HashMap<String, String>, name: &str, value: &str) {
    let key = normalize_key(name);
    let value = decode_xml(value).trim().to_string();
    if key.is_empty() || value.is_empty() {
        return;
    }
    fields.insert(key, value);
}

fn normalize_key(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn parse_description_fields(desc: &str, fields: &mut HashMap<String, String>) {
    let plain = decode_xml(desc)
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</tr>", "\n")
        .replace("</td>", " ")
        .replace("</th>", " ");
    let stripped = strip_tags(&plain);
    for line in stripped.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':').or_else(|| line.split_once('=')) else {
            continue;
        };
        insert_field(fields, key, value);
    }
}

fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn decode_xml(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn text_content(block: &str) -> String {
    decode_xml(&strip_tags(block)).trim().to_string()
}

fn first_tag_text(hay: &str, tag: &str) -> Option<String> {
    tagged_blocks(hay, tag)
        .into_iter()
        .next()
        .map(|inner| text_content(&inner))
}

fn tagged_block_start(hay: &str, tag: &str, inner: &str) -> Option<usize> {
    let inner_ptr = inner.as_ptr() as usize;
    let hay_ptr = hay.as_ptr() as usize;
    if inner_ptr < hay_ptr {
        return None;
    }
    let inner_off = inner_ptr - hay_ptr;
    let lower = hay.to_ascii_lowercase();
    let needle = tag.to_ascii_lowercase();
    let mut i = 0;
    while i < inner_off.min(lower.len()) {
        let Some(open) = find_open_tag(&lower, &needle, i) else {
            return None;
        };
        if let Some(gt) = lower[open..].find('>') {
            let content = open + gt + 1;
            if content == inner_off {
                return Some(open);
            }
            i = content;
        } else {
            return None;
        }
    }
    None
}

fn open_tag_attr(hay: &str, tag: &str, open_at: Option<usize>) -> Option<String> {
    let start = open_at.or_else(|| {
        let lower = hay.to_ascii_lowercase();
        find_open_tag(&lower, &tag.to_ascii_lowercase(), 0)
    })?;
    let rest = hay.get(start..)?;
    let gt = rest.find('>')?;
    let open = &rest[..gt];
    attr_value(open, "name")
}

fn attr_value(open_tag: &str, attr: &str) -> Option<String> {
    let lower = open_tag.to_ascii_lowercase();
    let key = format!("{attr}=");
    let at = lower.find(&key)?;
    let after = &open_tag[at + key.len()..];
    let quote = after.chars().next()?;
    if quote == '"' || quote == '\'' {
        let end = after[1..].find(quote)?;
        return Some(after[1..1 + end].to_string());
    }
    let end = after.find(|c: char| c.is_whitespace() || c == '>' || c == '/').unwrap_or(after.len());
    Some(after[..end].to_string())
}

fn tagged_blocks<'a>(text: &'a str, local_name: &str) -> Vec<&'a str> {
    let lower = text.to_ascii_lowercase();
    let needle = local_name.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lower.len() {
        let Some(start) = find_open_tag(&lower, &needle, i) else {
            break;
        };
        let Some(gt_rel) = lower[start..].find('>') else {
            break;
        };
        let gt = start + gt_rel;
        if lower.as_bytes().get(gt.saturating_sub(1)) == Some(&b'/') {
            i = gt + 1;
            continue;
        }
        let inner_start = gt + 1;
        if let Some(end) = find_close_tag(&lower, &needle, inner_start) {
            if end <= text.len() && inner_start <= end {
                out.push(&text[inner_start..end]);
            }
            i = end;
        } else {
            break;
        }
    }
    out
}

fn find_open_tag(lower: &str, local: &str, from: usize) -> Option<usize> {
    let mut i = from;
    while i < lower.len() {
        let rel = lower[i..].find('<')?;
        let at = i + rel;
        let after = at + 1;
        if lower.get(after..).is_some_and(|s| s.starts_with('/')) {
            i = after;
            continue;
        }
        if tag_local_name(&lower[after..]) == local {
            return Some(at);
        }
        i = after;
    }
    None
}

fn find_close_tag(lower: &str, local: &str, from: usize) -> Option<usize> {
    let mut i = from;
    let mut depth = 1;
    while i < lower.len() {
        let rel = lower[i..].find('<')?;
        let at = i + rel;
        let after = at + 1;
        let closing = lower.get(after..).is_some_and(|s| s.starts_with('/'));
        let name_src = if closing { &lower[after + 1..] } else { &lower[after..] };
        if tag_local_name(name_src) == local {
            if closing {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            } else if lower.as_bytes().get(after.saturating_sub(1)) != Some(&b'/') {
                let gt = lower[at..].find('>')?;
                if lower.as_bytes().get(at + gt - 1) != Some(&b'/') {
                    depth += 1;
                }
            }
        }
        i = after;
    }
    None
}

fn tag_local_name(after_lt: &str) -> String {
    let end = after_lt
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(after_lt.len());
    let raw = &after_lt[..end];
    raw.rsplit_once(':').map(|(_, n)| n.to_string()).unwrap_or_else(|| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2">
  <Document>
    <Placemark>
      <name>FV Gulf Star</name>
      <ExtendedData>
        <Data name="speed_kn"><value>8.4</value></Data>
        <Data name="heading_deg"><value>245</value></Data>
        <Data name="people_on_board"><value>6</value></Data>
        <Data name="length_m"><value>22</value></Data>
        <Data name="age_s"><value>45</value></Data>
      </ExtendedData>
      <Point><coordinates>-90.12,27.85,0</coordinates></Point>
    </Placemark>
    <Placemark>
      <name>MT Houston</name>
      <ExtendedData>
        <SchemaData>
          <SimpleData name="sog_kn">12.2</SimpleData>
          <SimpleData name="cog">095</SimpleData>
          <SimpleData name="pob">18</SimpleData>
          <SimpleData name="loa_m">180</SimpleData>
          <SimpleData name="age_min">2</SimpleData>
        </SchemaData>
      </ExtendedData>
      <Point><coordinates>-94.70,28.90</coordinates></Point>
    </Placemark>
  </Document>
</kml>"#;

    #[test]
    fn parses_extended_data_and_schema_data() {
        let boats = parse_kml(SAMPLE).unwrap();
        assert_eq!(boats.len(), 2);
        assert_eq!(boats[0].name, "FV Gulf Star");
        assert!((boats[0].lon + 90.12).abs() < 1e-9);
        assert!((boats[0].lat - 27.85).abs() < 1e-9);
        assert_eq!(boats[0].speed_kn, Some(8.4));
        assert_eq!(boats[0].heading_deg, Some(245.0));
        assert_eq!(boats[0].people_on_board, Some(6));
        assert_eq!(boats[0].length_m, Some(22.0));
        assert_eq!(boats[0].age_s, Some(45.0));
        assert_eq!(boats[1].name, "MT Houston");
        assert_eq!(boats[1].speed_kn, Some(12.2));
        assert_eq!(boats[1].heading_deg, Some(95.0));
        assert_eq!(boats[1].people_on_board, Some(18));
        assert_eq!(boats[1].length_m, Some(180.0));
        assert_eq!(boats[1].age_s, Some(120.0));
    }

    #[test]
    fn namespaced_kml_and_description_fallback() {
        let text = r#"<kml:kml><kml:Placemark>
          <kml:name>Rec 1</kml:name>
          <kml:description>speed: 6 kn
heading: 10
people: 4
length: 9 m
age: 30 s</kml:description>
          <kml:Point><kml:coordinates>-83.1,27.9,0</kml:coordinates></kml:Point>
        </kml:Placemark></kml:kml>"#;
        let boats = parse_kml(text).unwrap();
        assert_eq!(boats[0].speed_kn, Some(6.0));
        assert_eq!(boats[0].heading_deg, Some(10.0));
        assert_eq!(boats[0].people_on_board, Some(4));
        assert_eq!(boats[0].length_m, Some(9.0));
        assert_eq!(boats[0].age_s, Some(30.0));
    }

    #[test]
    fn sample_gulf_kml_files_have_required_fields() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("samples")
            .join("boats");
        let mut total = 0;
        for name in ["gom_fishing.kml", "gom_shipping.kml", "gom_recreational.kml"] {
            let boats = parse_kml_path(&dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(boats.len() >= 6, "{name} had {}", boats.len());
            for boat in &boats {
                assert!(boat.speed_kn.is_some(), "{name} {} missing speed", boat.name);
                assert!(boat.heading_deg.is_some(), "{name} {} missing heading", boat.name);
                assert!(
                    boat.people_on_board.is_some(),
                    "{name} {} missing people",
                    boat.name
                );
                assert!(boat.length_m.is_some(), "{name} {} missing size", boat.name);
                assert!(boat.age_s.is_some(), "{name} {} missing age", boat.name);
            }
            total += boats.len();
        }
        assert!(total >= 20, "expected a full Gulf sample set, got {total}");
    }

    #[test]
    fn dead_reckon_moves_east() {
        let boat = Boat {
            id: 1,
            name: "t".into(),
            lon: -90.0,
            lat: 0.0,
            alt_m: 0.0,
            speed_kn: Some(10.0),
            heading_deg: Some(90.0),
            people_on_board: Some(1),
            length_m: Some(10.0),
            age_s: Some(3600.0),
            source: None,
            visible: true,
            color: "#00ffcc".into(),
        };
        let view = boat.view();
        assert!(view.estimate_lon > boat.lon);
        assert!(view.uncertainty_m > 18_000.0);
        assert!(view.predict_lla.len() >= 9);
    }

    #[test]
    fn boat_on_kde_peak_scores_higher_than_far_boat() {
        let mut pts = Vec::new();
        for i in 0..40 {
            let jitter = (i as f64 - 20.0) * 0.002;
            pts.push(crate::kde::WeightedPoint {
                lon: -90.0 + jitter,
                lat: 28.0 + jitter * 0.3,
                weight: 0.025,
            });
        }
        let grid = crate::kde::kde_lonlat(&pts).expect("kde");
        let hot = score_boat(
            Boat {
                id: 1,
                name: "hot".into(),
                lon: -90.0,
                lat: 28.0,
                alt_m: 0.0,
                speed_kn: Some(2.0),
                heading_deg: Some(90.0),
                people_on_board: Some(10),
                length_m: Some(20.0),
                age_s: Some(10.0),
                source: None,
                visible: true,
                color: "#0ff".into(),
            }
            .view(),
            &grid,
        );
        let cold = score_boat(
            Boat {
                id: 2,
                name: "cold".into(),
                lon: -80.0,
                lat: 20.0,
                alt_m: 0.0,
                speed_kn: Some(2.0),
                heading_deg: Some(90.0),
                people_on_board: Some(10),
                length_m: Some(20.0),
                age_s: Some(10.0),
                source: None,
                visible: true,
                color: "#0ff".into(),
            }
            .view(),
            &grid,
        );
        assert!(hot.p_hit.unwrap() > cold.p_hit.unwrap());
        assert!(hot.expected_casualties.unwrap() > cold.expected_casualties.unwrap());
        assert!((hot.expected_casualties.unwrap() - hot.p_hit.unwrap() * 10.0).abs() < 1e-12);
    }
}
