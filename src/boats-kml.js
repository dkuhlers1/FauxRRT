const SPEED_KEYS = ["speed_kn", "speed_kts", "sog_kn", "sog", "speed"];
const HEADING_KEYS = ["heading_deg", "heading", "cog_deg", "cog", "course"];
const PEOPLE_KEYS = ["people_on_board", "pob", "people", "souls", "persons"];
const LENGTH_KEYS = ["length_m", "loa_m", "size_m", "length", "size", "loa"];
const LENGTH_FT_KEYS = ["length_ft", "loa_ft"];
const AGE_KEYS = ["age_s", "time_since_update_s", "last_update_age_s", "stale_s", "seconds_since_update", "age"];
const AGE_MIN_KEYS = ["age_min", "minutes_since_update"];

let nextFallbackId = 1;

export function parseBoatKml(text, sourceName = "") {
  const doc = new DOMParser().parseFromString(text, "text/xml");
  if (doc.querySelector("parsererror")) {
    throw new Error("invalid KML");
  }
  const marks = [...doc.getElementsByTagName("*")].filter((el) => localName(el) === "placemark");
  const boats = [];
  marks.forEach((mark, i) => {
    const coord = lastCoordinate(mark);
    if (!coord) return;
    const fields = extendedFields(mark);
    boats.push(enrichBoat({
      id: nextFallbackId++,
      name: firstText(mark, "name") || `Boat ${i + 1}`,
      lon: coord.lon,
      lat: coord.lat,
      alt_m: coord.alt,
      speed_kn: firstNumber(fields, SPEED_KEYS),
      heading_deg: firstNumber(fields, HEADING_KEYS),
      people_on_board: firstInt(fields, PEOPLE_KEYS),
      length_m: firstNumber(fields, LENGTH_KEYS) ?? feetToM(firstNumber(fields, LENGTH_FT_KEYS)),
      age_s: firstNumber(fields, AGE_KEYS) ?? minutesToS(firstNumber(fields, AGE_MIN_KEYS)),
      source: sourceName || null,
      visible: true,
      color: boatColor(nextFallbackId - 1),
    }));
  });
  if (!boats.length) throw new Error("KML had no usable boat placemarks");
  return boats;
}

export function enrichBoat(boat) {
  const predict = deadReckon(boat);
  return { ...boat, ...predict };
}

export function scoreBoatsAgainstGrid(list, grid) {
  if (!grid?.values?.length) {
    return list.map((boat) => ({
      ...enrichBoat(boat),
      area_m2: null,
      kde_density: null,
      p_hit: null,
      p_hit_mean: null,
      expected_casualties: null,
    }));
  }
  return list.map((boat) => scoreBoatAgainstGrid(enrichBoat(boat), grid));
}

function scoreBoatAgainstGrid(boat, grid) {
  const samples = [[boat.lon, boat.lat], [boat.estimate_lon, boat.estimate_lat]];
  for (let i = 0; i + 1 < (boat.predict_lla || []).length; i += 3) {
    samples.push([boat.predict_lla[i], boat.predict_lla[i + 1]]);
  }
  if (boat.uncertainty_m > 50) {
    for (let i = 0; i < 8; i++) {
      const p = destination(boat.lon, boat.lat, i * 45, boat.uncertainty_m);
      samples.push([p.lon, p.lat]);
    }
  }
  const dens = samples.map(([lon, lat]) => Math.max(0, sampleGrid(grid, lon, lat)));
  const peak = dens.reduce((m, v) => Math.max(m, v), 0);
  const mean = dens.length ? dens.reduce((s, v) => s + v, 0) / dens.length : 0;
  const length = Number(boat.length_m);
  const area = (Number.isFinite(length) && length > 0 ? length : 12) * Math.max((Number.isFinite(length) && length > 0 ? length : 12) / 4, 3);
  const pHit = 1 - Math.exp(-Math.min(50, peak * area));
  const pMean = 1 - Math.exp(-Math.min(50, mean * area));
  return {
    ...boat,
    area_m2: area,
    kde_density: peak,
    p_hit: pHit,
    p_hit_mean: pMean,
    expected_casualties: boat.people_on_board != null ? pHit * Number(boat.people_on_board) : null,
  };
}

export function sampleGrid(grid, lon, lat) {
  if (!grid || grid.nx < 2 || grid.ny < 2) return 0;
  const dw = grid.east - grid.west;
  const dh = grid.north - grid.south;
  if (dw <= 0 || dh <= 0) return 0;
  const fx = ((lon - grid.west) / dw) * grid.nx - 0.5;
  const fy = ((grid.north - lat) / dh) * grid.ny - 0.5;
  if (fx < 0 || fy < 0 || fx > grid.nx - 1 || fy > grid.ny - 1) return 0;
  const i0 = Math.floor(fx);
  const j0 = Math.floor(fy);
  const i1 = Math.min(i0 + 1, grid.nx - 1);
  const j1 = Math.min(j0 + 1, grid.ny - 1);
  const tx = fx - i0;
  const ty = fy - j0;
  const v = (i, j) => grid.values[j * grid.nx + i] || 0;
  return (1 - ty) * ((1 - tx) * v(i0, j0) + tx * v(i1, j0)) + ty * ((1 - tx) * v(i0, j1) + tx * v(i1, j1));
}

function deadReckon(boat) {
  const lla = [boat.lon, boat.lat, boat.alt_m || 0];
  const speed = Number(boat.speed_kn);
  const heading = Number(boat.heading_deg);
  if (!Number.isFinite(speed) || speed < 0 || !Number.isFinite(heading)) {
    return { estimate_lon: boat.lon, estimate_lat: boat.lat, uncertainty_m: 0, predict_lla: lla };
  }
  const speedMps = (speed * 1852) / 3600;
  const age = Number(boat.age_s);
  const ageS = Number.isFinite(age) && age > 0 ? age : 0;
  const now = destination(boat.lon, boat.lat, heading, speedMps * ageS);
  const predict = [...lla, now.lon, now.lat, boat.alt_m || 0];
  for (let i = 1; i <= 5; i++) {
    const t = 3600 * (i / 5);
    const p = destination(now.lon, now.lat, heading, speedMps * t);
    predict.push(p.lon, p.lat, boat.alt_m || 0);
  }
  return {
    estimate_lon: now.lon,
    estimate_lat: now.lat,
    uncertainty_m: speedMps * ageS,
    predict_lla: predict,
  };
}

function destination(lon, lat, headingDeg, distM) {
  if (Math.abs(distM) < 1e-6) return { lon, lat };
  const r = 6371000;
  const brng = (headingDeg * Math.PI) / 180;
  const lat1 = (lat * Math.PI) / 180;
  const lon1 = (lon * Math.PI) / 180;
  const ang = distM / r;
  const lat2 = Math.asin(Math.sin(lat1) * Math.cos(ang) + Math.cos(lat1) * Math.sin(ang) * Math.cos(brng));
  const lon2 =
    lon1 +
    Math.atan2(
      Math.sin(brng) * Math.sin(ang) * Math.cos(lat1),
      Math.cos(ang) - Math.sin(lat1) * Math.sin(lat2),
    );
  let lonDeg = (lon2 * 180) / Math.PI;
  if (lonDeg > 180) lonDeg -= 360;
  if (lonDeg < -180) lonDeg += 360;
  return { lon: lonDeg, lat: (lat2 * 180) / Math.PI };
}

function extendedFields(mark) {
  const fields = {};
  for (const el of mark.getElementsByTagName("*")) {
    const name = localName(el);
    if (name !== "data" && name !== "simpledata") continue;
    const key = normalizeKey(el.getAttribute("name") || "");
    const value =
      name === "data"
        ? firstText(el, "value") || (el.textContent || "").trim()
        : (el.textContent || "").trim();
    if (key && value) fields[key] = value;
  }
  if (!Object.keys(fields).length) {
    const desc = firstText(mark, "description");
    if (desc) parseDescription(desc, fields);
  }
  return fields;
}

function parseDescription(desc, fields) {
  const plain = desc.replace(/<br\s*\/?>/gi, "\n").replace(/<\/tr>/gi, "\n").replace(/<[^>]+>/g, " ");
  for (const line of plain.split("\n")) {
    const parts = line.split(/[:=]/);
    if (parts.length < 2) continue;
    const key = normalizeKey(parts[0]);
    const value = parts.slice(1).join(":").trim();
    if (key && value) fields[key] = value;
  }
}

function lastCoordinate(mark) {
  const nodes = [...mark.getElementsByTagName("*")].filter((el) => localName(el) === "coordinates");
  let last = null;
  for (const node of nodes) {
    const text = (node.textContent || "").trim();
    for (const token of text.split(/\s+/)) {
      const [lon, lat, alt] = token.split(",").map(Number);
      if (Number.isFinite(lon) && Number.isFinite(lat)) {
        last = { lon, lat, alt: Number.isFinite(alt) ? alt : 0 };
      }
    }
  }
  return last;
}

function firstText(root, tag) {
  const el = [...root.getElementsByTagName("*")].find((node) => localName(node) === tag);
  return el ? el.textContent.trim() : "";
}

function firstNumber(fields, keys) {
  for (const key of keys) {
    const raw = fields[key];
    if (raw == null) continue;
    const n = Number(String(raw).trim().split(/[\s,;]+/)[0]);
    if (Number.isFinite(n)) return n;
  }
  return null;
}

function firstInt(fields, keys) {
  const n = firstNumber(fields, keys);
  return n == null ? null : Math.max(0, Math.round(n));
}

function feetToM(ft) {
  return ft == null ? null : ft * 0.3048;
}

function minutesToS(min) {
  return min == null ? null : min * 60;
}

function normalizeKey(name) {
  return String(name)
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

function localName(el) {
  return (el.localName || el.tagName || "").replace(/^.*:/, "").toLowerCase();
}

function boatColor(id) {
  const h = 0.47 + ((id * 0.6180339887498949) % 1) * 0.08;
  const [r, g, b] = hslToRgb(h, 0.68, 0.56);
  return `#${hex(r)}${hex(g)}${hex(b)}`;
}

function hslToRgb(h, s, l) {
  const a = s * Math.min(l, 1 - l);
  const f = (n) => {
    const k = (n + h * 12) % 12;
    const v = l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(v * 255);
  };
  return [f(0), f(8), f(4)];
}

function hex(n) {
  return n.toString(16).padStart(2, "0");
}
