use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frame {
    Lla,
    Ecef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnRole {
    Time,
    Lat,
    Lon,
    Alt,
    X,
    Y,
    Z,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub index: usize,
    pub name: String,
    pub role: ColumnRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedSchema {
    pub delimiter: String,
    pub has_header: bool,
    pub frame: Frame,
    pub confidence: f32,
    pub columns: Vec<ColumnInfo>,
    pub time_col: Option<usize>,
    pub lat_col: Option<usize>,
    pub lon_col: Option<usize>,
    pub alt_col: Option<usize>,
    pub x_col: Option<usize>,
    pub y_col: Option<usize>,
    pub z_col: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnMapping {
    pub time_col: Option<usize>,
    pub lat_col: Option<usize>,
    pub lon_col: Option<usize>,
    pub alt_col: Option<usize>,
    pub x_col: Option<usize>,
    pub y_col: Option<usize>,
    pub z_col: Option<usize>,
}

impl DetectedSchema {
    pub fn apply_mapping(&mut self, mapping: &ColumnMapping) {
        self.time_col = mapping.time_col;
        self.lat_col = mapping.lat_col;
        self.lon_col = mapping.lon_col;
        self.alt_col = mapping.alt_col;
        self.x_col = mapping.x_col;
        self.y_col = mapping.y_col;
        self.z_col = mapping.z_col;
        if self.x_col.is_some() && self.y_col.is_some() && self.z_col.is_some() {
            self.frame = Frame::Ecef;
        } else {
            self.frame = Frame::Lla;
        }
        for col in &mut self.columns {
            col.role = role_from_mapping(col.index, mapping);
        }
    }

    pub fn mapping(&self) -> ColumnMapping {
        ColumnMapping {
            time_col: self.time_col,
            lat_col: self.lat_col,
            lon_col: self.lon_col,
            alt_col: self.alt_col,
            x_col: self.x_col,
            y_col: self.y_col,
            z_col: self.z_col,
        }
    }

    pub fn is_usable(&self) -> bool {
        match self.frame {
            Frame::Lla => self.lat_col.is_some() && self.lon_col.is_some(),
            Frame::Ecef => self.x_col.is_some() && self.y_col.is_some() && self.z_col.is_some(),
        }
    }

    pub fn generated() -> Self {
        Self {
            delimiter: "csv".into(),
            has_header: true,
            frame: Frame::Lla,
            confidence: 1.0,
            columns: vec![
                ColumnInfo { index: 0, name: "time".into(), role: ColumnRole::Time },
                ColumnInfo { index: 1, name: "lat".into(), role: ColumnRole::Lat },
                ColumnInfo { index: 2, name: "lon".into(), role: ColumnRole::Lon },
                ColumnInfo { index: 3, name: "alt".into(), role: ColumnRole::Alt },
            ],
            time_col: Some(0),
            lat_col: Some(1),
            lon_col: Some(2),
            alt_col: Some(3),
            x_col: None,
            y_col: None,
            z_col: None,
        }
    }
}

fn role_from_mapping(index: usize, mapping: &ColumnMapping) -> ColumnRole {
    if mapping.time_col == Some(index) {
        ColumnRole::Time
    } else if mapping.lat_col == Some(index) {
        ColumnRole::Lat
    } else if mapping.lon_col == Some(index) {
        ColumnRole::Lon
    } else if mapping.alt_col == Some(index) {
        ColumnRole::Alt
    } else if mapping.x_col == Some(index) {
        ColumnRole::X
    } else if mapping.y_col == Some(index) {
        ColumnRole::Y
    } else if mapping.z_col == Some(index) {
        ColumnRole::Z
    } else {
        ColumnRole::Other
    }
}

#[derive(Debug, Clone)]
pub struct DelimitedPreview {
    pub delimiter: Delimiter,
    pub has_header: bool,
    pub names: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delimiter {
    Comma,
    Tab,
    Semicolon,
    Pipe,
    Whitespace,
}

impl Delimiter {
    pub fn as_label(self) -> &'static str {
        match self {
            Delimiter::Comma => "comma",
            Delimiter::Tab => "tab",
            Delimiter::Semicolon => "semicolon",
            Delimiter::Pipe => "pipe",
            Delimiter::Whitespace => "whitespace",
        }
    }

    pub fn split(self, line: &str) -> Vec<String> {
        match self {
            Delimiter::Whitespace => line.split_whitespace().map(|s| s.to_string()).collect(),
            other => {
                let ch = match other {
                    Delimiter::Comma => ',',
                    Delimiter::Tab => '\t',
                    Delimiter::Semicolon => ';',
                    Delimiter::Pipe => '|',
                    Delimiter::Whitespace => unreachable!(),
                };
                line.split(ch).map(|s| s.trim().to_string()).collect()
            }
        }
    }
}

pub fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty()
        || t.starts_with('#')
        || t.starts_with('%')
        || t.starts_with('!')
        || t.starts_with("//")
        || t.starts_with(';') && !t.contains(',')
        || t.starts_with("BEGIN")
        || t.starts_with("END")
        || starts_ignore_ascii(t, "stk.")
        || starts_ignore_ascii(t, "scenario")
        || starts_ignore_ascii(t, "ephemeris")
}

fn starts_ignore_ascii(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix)
}

pub fn detect_preview(text: &str) -> Option<DelimitedPreview> {
    let raw: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !is_comment_line(l))
        .take(80)
        .collect();
    let data: Vec<&str> = raw
        .iter()
        .copied()
        .filter(|l| l.chars().any(|c| c.is_ascii_digit()))
        .collect();
    if data.len() < 2 {
        return None;
    }

    let delimiter = detect_delimiter(&data)?;
    let mut lines = Vec::new();
    if let Some(first) = raw.first() {
        if !first.chars().any(|c| c.is_ascii_digit()) {
            lines.push(*first);
        }
    }
    lines.extend(data);
    let parsed: Vec<Vec<String>> = lines.iter().map(|l| delimiter.split(l)).collect();
    let width = most_common_width(&parsed);
    let parsed: Vec<Vec<String>> = parsed.into_iter().filter(|r| r.len() == width).collect();
    if parsed.len() < 2 || width < 2 {
        return None;
    }

    let has_header = row_is_header(&parsed[0]);
    let names = if has_header {
        parsed[0]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                if name.is_empty() {
                    format!("col_{i}")
                } else {
                    name.clone()
                }
            })
            .collect()
    } else {
        (0..width).map(|i| format!("col_{i}")).collect()
    };
    let rows = if has_header {
        parsed[1..].to_vec()
    } else {
        parsed
    };

    Some(DelimitedPreview {
        delimiter,
        has_header,
        names,
        rows,
    })
}

pub fn detect_schema(preview: &DelimitedPreview) -> DetectedSchema {
    let width = preview.names.len();
    let samples: Vec<Vec<ColumnSample>> = (0..width)
        .map(|i| preview.rows.iter().filter_map(|row| row.get(i)).map(|v| sample_field(v)).collect())
        .collect();

    let mut roles = vec![ColumnRole::Other; width];
    let mut confidence: f32 = 0.35;

    for (i, name) in preview.names.iter().enumerate() {
        if let Some(role) = role_from_name(name) {
            roles[i] = role;
            confidence = confidence.max(0.86);
        }
    }

    let mut time_col = roles.iter().position(|r| *r == ColumnRole::Time);
    let mut lat_col = roles.iter().position(|r| *r == ColumnRole::Lat);
    let mut lon_col = roles.iter().position(|r| *r == ColumnRole::Lon);
    let mut alt_col = roles.iter().position(|r| *r == ColumnRole::Alt);
    let mut x_col = roles.iter().position(|r| *r == ColumnRole::X);
    let mut y_col = roles.iter().position(|r| *r == ColumnRole::Y);
    let mut z_col = roles.iter().position(|r| *r == ColumnRole::Z);

    if time_col.is_none() {
        time_col = infer_time_column(&samples);
        if let Some(i) = time_col {
            roles[i] = ColumnRole::Time;
        }
    }

    let named_ecef = x_col.is_some() && y_col.is_some() && z_col.is_some();
    let named_lla = lat_col.is_some() && lon_col.is_some();

    if !named_lla && !named_ecef {
        if let Some((x, y, z)) = infer_ecef(&samples, time_col) {
            x_col = Some(x);
            y_col = Some(y);
            z_col = Some(z);
            roles[x] = ColumnRole::X;
            roles[y] = ColumnRole::Y;
            roles[z] = ColumnRole::Z;
            confidence = confidence.max(0.78);
        } else if let Some((lat, lon, alt)) = infer_lla(&samples, time_col) {
            lat_col = Some(lat);
            lon_col = Some(lon);
            alt_col = alt;
            roles[lat] = ColumnRole::Lat;
            roles[lon] = ColumnRole::Lon;
            if let Some(a) = alt {
                roles[a] = ColumnRole::Alt;
            }
            confidence = confidence.max(0.74);
        }
    } else if named_lla {
        confidence = confidence.max(0.9);
    } else if named_ecef {
        confidence = confidence.max(0.9);
    }

    let frame = if x_col.is_some() && y_col.is_some() && z_col.is_some() && !named_lla {
        Frame::Ecef
    } else {
        Frame::Lla
    };

    let columns = preview
        .names
        .iter()
        .enumerate()
        .map(|(index, name)| ColumnInfo {
            index,
            name: name.clone(),
            role: roles[index],
        })
        .collect();

    DetectedSchema {
        delimiter: preview.delimiter.as_label().to_string(),
        has_header: preview.has_header,
        frame,
        confidence,
        columns,
        time_col,
        lat_col,
        lon_col,
        alt_col,
        x_col,
        y_col,
        z_col,
    }
}

fn detect_delimiter(lines: &[&str]) -> Option<Delimiter> {
    let candidates = [
        Delimiter::Tab,
        Delimiter::Comma,
        Delimiter::Semicolon,
        Delimiter::Pipe,
        Delimiter::Whitespace,
    ];
    let mut best: Option<(Delimiter, i32)> = None;
    for delim in candidates {
        let widths: Vec<usize> = lines.iter().map(|l| delim.split(l).len()).collect();
        let width = most_common_width_usize(&widths);
        if width < 2 {
            continue;
        }
        let matches = widths.iter().filter(|w| **w == width).count();
        let score = (matches as i32) * 10 + width as i32
            + match delim {
                Delimiter::Tab => 3,
                Delimiter::Comma => 2,
                Delimiter::Semicolon => 1,
                Delimiter::Pipe => 1,
                Delimiter::Whitespace => 0,
            };
        if best.map(|(_, s)| score > s).unwrap_or(true) {
            best = Some((delim, score));
        }
    }
    best.map(|(d, _)| d)
}

fn most_common_width(rows: &[Vec<String>]) -> usize {
    most_common_width_usize(&rows.iter().map(|r| r.len()).collect::<Vec<_>>())
}

fn most_common_width_usize(widths: &[usize]) -> usize {
    let mut counts = [0usize; 64];
    for w in widths {
        if *w < counts.len() {
            counts[*w] += 1;
        }
    }
    counts
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| *c)
        .map(|(w, _)| w)
        .unwrap_or(0)
}

pub fn row_is_header(row: &[String]) -> bool {
    if row.is_empty() {
        return false;
    }
    let non_numeric = row.iter().filter(|c| !is_numeric_field(c) && !c.is_empty()).count();
    non_numeric * 2 >= row.len()
}

fn is_numeric_field(value: &str) -> bool {
    parse_number(value).is_some()
}

pub fn parse_number(value: &str) -> Option<f64> {
    let v = value.trim().trim_matches(|c| c == '"' || c == '\'');
    if v.is_empty() {
        return None;
    }
    v.parse::<f64>().ok()
}

#[derive(Clone, Copy)]
struct ColumnSample {
    number: Option<f64>,
    datetime: bool,
}

fn sample_field(value: &str) -> ColumnSample {
    ColumnSample {
        number: parse_number(value),
        datetime: looks_like_datetime(value),
    }
}

fn looks_like_datetime(value: &str) -> bool {
    let v = value.trim();
    (v.len() >= 8 && v.contains('-') && (v.contains('T') || v.contains(':')))
        || (v.contains('/') && v.contains(':'))
}

fn role_from_name(name: &str) -> Option<ColumnRole> {
    let n = normalize_name(name);
    const TIME: &[&str] = &[
        "t", "time", "epoch", "utc", "gps", "gpst", "met", "sow", "datetime", "timestamp",
        "utc_time", "gps_time", "time_utc", "time_s", "seconds", "sec", "elapsed",
    ];
    const LAT: &[&str] = &["lat", "latitude", "geodetic_lat", "geodetic_latitude", "lat_deg", "latd"];
    const LON: &[&str] = &[
        "lon", "long", "lng", "longitude", "geodetic_lon", "geodetic_longitude", "lon_deg", "lond",
    ];
    const ALT: &[&str] = &[
        "alt", "altitude", "height", "hae", "msl", "elev", "elevation", "ht", "alt_m", "height_m",
        "alt_km",
    ];
    const X: &[&str] = &["x", "x_ecef", "ecef_x", "eci_x", "pos_x", "x_m", "x_km"];
    const Y: &[&str] = &["y", "y_ecef", "ecef_y", "eci_y", "pos_y", "y_m", "y_km"];
    const Z: &[&str] = &["z", "z_ecef", "ecef_z", "eci_z", "pos_z", "z_m", "z_km"];

    if TIME.contains(&n.as_str()) {
        return Some(ColumnRole::Time);
    }
    if LAT.contains(&n.as_str()) {
        return Some(ColumnRole::Lat);
    }
    if LON.contains(&n.as_str()) {
        return Some(ColumnRole::Lon);
    }
    if ALT.contains(&n.as_str()) {
        return Some(ColumnRole::Alt);
    }
    if X.contains(&n.as_str()) {
        return Some(ColumnRole::X);
    }
    if Y.contains(&n.as_str()) {
        return Some(ColumnRole::Y);
    }
    if Z.contains(&n.as_str()) {
        return Some(ColumnRole::Z);
    }
    None
}

fn normalize_name(name: &str) -> String {
    name.trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn infer_time_column(samples: &[Vec<ColumnSample>]) -> Option<usize> {
    let mut best: Option<(usize, i32)> = None;
    for (i, col) in samples.iter().enumerate() {
        let dt = col.iter().filter(|s| s.datetime).count();
        if dt * 2 >= col.len() && !col.is_empty() {
            return Some(i);
        }
        let nums: Vec<f64> = col.iter().filter_map(|s| s.number).collect();
        if nums.len() < 3 || !looks_like_clock(&nums) {
            continue;
        }
        let mut score = 2;
        if is_mostly_monotonic(&nums) {
            score += 4;
        }
        let min = nums.iter().copied().fold(f64::INFINITY, f64::min);
        if (1.0e9..2.2e9).contains(&min) || (1.0e12..2.2e12).contains(&min) {
            score += 5;
        }
        if best.map(|(_, s)| score > s).unwrap_or(true) {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
}

fn looks_like_clock(nums: &[f64]) -> bool {
    let min = nums.iter().copied().fold(f64::INFINITY, f64::min);
    let max = nums.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if (1.0e9..2.2e9).contains(&min) || (1.0e12..2.2e12).contains(&min) {
        return true;
    }
    min >= 0.0 && min < 1000.0 && max < 1.0e8 && is_mostly_monotonic(nums)
}

fn infer_ecef(samples: &[Vec<ColumnSample>], skip: Option<usize>) -> Option<(usize, usize, usize)> {
    let idxs: Vec<usize> = samples
        .iter()
        .enumerate()
        .filter(|(i, col)| Some(*i) != skip && col.iter().filter_map(|s| s.number).count() >= 3)
        .map(|(i, _)| i)
        .collect();
    let mut best: Option<((usize, usize, usize), f64)> = None;
    for window in idxs.windows(3) {
        let (i, j, k) = (window[0], window[1], window[2]);
        let xs: Vec<f64> = samples[i].iter().filter_map(|s| s.number).collect();
        let ys: Vec<f64> = samples[j].iter().filter_map(|s| s.number).collect();
        let zs: Vec<f64> = samples[k].iter().filter_map(|s| s.number).collect();
        let n = xs.len().min(ys.len()).min(zs.len());
        if n < 3 {
            continue;
        }
        let mean_r = (0..n)
            .map(|t| (xs[t] * xs[t] + ys[t] * ys[t] + zs[t] * zs[t]).sqrt())
            .sum::<f64>()
            / n as f64;
        if (6.0e6..8.0e7).contains(&mean_r) {
            let score = (mean_r - 6_371_000.0).abs();
            if best.map(|(_, s)| score < s).unwrap_or(true) {
                best = Some(((i, j, k), score));
            }
        }
    }
    best.map(|(ijk, _)| ijk)
}

fn infer_lla(
    samples: &[Vec<ColumnSample>],
    skip: Option<usize>,
) -> Option<(usize, usize, Option<usize>)> {
    let mut stats = Vec::new();
    for (i, col) in samples.iter().enumerate() {
        if Some(i) == skip {
            continue;
        }
        let nums: Vec<f64> = col.iter().filter_map(|s| s.number).collect();
        if nums.len() < 3 {
            continue;
        }
        let min = nums.iter().copied().fold(f64::INFINITY, f64::min);
        let max = nums.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        stats.push((i, min, max, max - min));
    }

    let lon = stats
        .iter()
        .filter(|(_, min, max, span)| *span > 0.01 && *min >= -180.0 && *max <= 180.0)
        .max_by(|a, b| lon_score(a).partial_cmp(&lon_score(b)).unwrap())
        .map(|s| s.0)?;
    let lat = stats
        .iter()
        .filter(|(i, min, max, span)| {
            *i != lon && *span > 0.01 && *min >= -90.0 && *max <= 90.0 && *span <= 180.0
        })
        .max_by(|a, b| a.3.partial_cmp(&b.3).unwrap())
        .map(|s| s.0)?;
    let alt = stats
        .iter()
        .map(|s| s.0)
        .find(|i| *i != lat && *i != lon && looks_like_alt(stats.iter().find(|s| s.0 == *i).unwrap()));
    Some((lat, lon, alt))
}

fn lon_score((_, min, max, span): &(usize, f64, f64, f64)) -> f64 {
    let mut score = *span;
    if *min < -90.0 {
        score += 80.0;
    }
    if *max > 90.0 && *min < 0.0 {
        score += 30.0;
    }
    if *min > 90.0 {
        score -= 90.0;
    }
    score
}

fn looks_like_alt((_, min, max, span): &(usize, f64, f64, f64)) -> bool {
    *span < 2.0e6 && (*max > 180.0 || min.abs() > 200.0 || (*min >= -500.0 && *max < 2.0e5))
}

fn is_mostly_monotonic(values: &[f64]) -> bool {
    let mut inc = 0;
    let mut dec = 0;
    for w in values.windows(2) {
        if w[1] >= w[0] {
            inc += 1;
        } else {
            dec += 1;
        }
    }
    inc + dec > 0 && inc.max(dec) * 5 >= (inc + dec) * 4
}

pub fn parse_time_value(raw: &str) -> Option<f64> {
    if let Some(n) = parse_number(raw) {
        if (1.0e12..2.2e12).contains(&n) {
            return Some(n / 1000.0);
        }
        return Some(n);
    }
    parse_datetime(raw)
}

fn parse_datetime(raw: &str) -> Option<f64> {
    let v = raw.trim().replace('T', " ");
    let v = v.trim_end_matches('Z');
    let mut parts = v.split([' ', 'T']);
    let date = parts.next()?;
    let time = parts.next().unwrap_or("00:00:00");
    let sep = if date.contains('-') { '-' } else { '/' };
    let mut dp = date.split(sep);
    let y: i32 = dp.next()?.parse().ok()?;
    let m: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let hh: u32 = tp.next()?.parse().ok()?;
    let mm: u32 = tp.next()?.parse().ok()?;
    let ss: f64 = tp.next().unwrap_or("0").parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(unix_from_ymd_hms(y, m, d, hh, mm, ss))
}

fn unix_from_ymd_hms(year: i32, month: u32, day: u32, hh: u32, mm: u32, ss: f64) -> f64 {
    let mut y = year;
    let mut m = month as i32;
    if m <= 2 {
        y -= 1;
        m += 12;
    }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = ((153 * (m - 3) + 2) / 5) as u64 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era as i64) * 146097 + doe as i64 - 719468;
    (days as f64) * 86400.0 + (hh as f64) * 3600.0 + (mm as f64) * 60.0 + ss
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_lla_csv() {
        let text = "time,lat,lon,alt\n0,37.7,-122.4,100\n1,37.8,-122.3,120\n2,37.9,-122.2,140\n";
        let preview = detect_preview(text).unwrap();
        let schema = detect_schema(&preview);
        assert_eq!(schema.frame, Frame::Lla);
        assert_eq!(schema.lat_col, Some(1));
        assert_eq!(schema.lon_col, Some(2));
    }

    #[test]
    fn detects_ecef_whitespace() {
        let text = "X Y Z\n6378137 0 0\n6378130 10000 2000\n6378120 20000 4000\n";
        let preview = detect_preview(text).unwrap();
        let schema = detect_schema(&preview);
        assert_eq!(schema.frame, Frame::Ecef);
        assert_eq!(schema.x_col, Some(0));
    }
}
