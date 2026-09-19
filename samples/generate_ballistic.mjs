import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const G = 9.80665;
const DEG = Math.PI / 180;
const R_EARTH = 6378137;

const GULF = {
  latMin: 18.6,
  latMax: 30.4,
  lonMin: -97.7,
  lonMax: -80.6,
};

const outDir = join(dirname(fileURLToPath(import.meta.url)), "ballistic");
await mkdir(outDir, { recursive: true });

const rand = mulberry32(20260829);

const COUNT = 1000;
let written = 0;
let attempt = 0;
while (written < COUNT && attempt < COUNT * 20) {
  attempt += 1;
  const launch = randomGulfPoint(rand);
  const rangeKm = lerp(45, 980, Math.pow(rand(), 0.85));
  const az = rand() * 360;
  const impact = destination(launch.lat, launch.lon, az, rangeKm * 1000);
  if (!inGulf(impact.lat, impact.lon)) continue;

  const elevDeg = lerp(32, 54, rand());
  const track = vacuumBallistic(launch, impact, elevDeg, rangeKm * 1000);
  if (!track) continue;

  const id = String(written + 1).padStart(4, "0");
  const header = "time,lat,lon,alt,ve,vn,vu";
  const body = track
    .map(
      (p) =>
        `${p.t.toFixed(2)},${p.lat.toFixed(6)},${p.lon.toFixed(6)},${p.alt.toFixed(1)},${p.ve.toFixed(2)},${p.vn.toFixed(2)},${p.vu.toFixed(2)}`,
    )
    .join("\n");
  await writeFile(join(outDir, `gom_ballistic_${id}.csv`), `${header}\n${body}\n`);
  written += 1;
}

if (written < COUNT) {
  throw new Error(`only generated ${written} trajectories`);
}
console.log(`wrote ${written} ballistic tracks to ${outDir}`);

function vacuumBallistic(launch, impact, elevDeg, rangeM) {
  const elev = elevDeg * DEG;
  const v0 = Math.sqrt((rangeM * G) / Math.sin(2 * elev));
  const tFlight = (2 * v0 * Math.sin(elev)) / G;
  if (!Number.isFinite(tFlight) || tFlight < 20 || tFlight > 1800) return null;

  const n = Math.max(120, Math.min(420, Math.round(tFlight / 0.75)));
  const points = [];
  for (let i = 0; i < n; i += 1) {
    const u = i / (n - 1);
    const t = u * tFlight;
    const downrange = v0 * Math.cos(elev) * t;
    const alt = Math.max(0, v0 * Math.sin(elev) * t - 0.5 * G * t * t);
    const frac = Math.min(1, downrange / rangeM);
    const { lat, lon } = interpolateGc(launch.lat, launch.lon, impact.lat, impact.lon, frac);
    const ve = horizontalSpeed(v0, elev) * Math.sin(bearing(launch.lat, launch.lon, impact.lat, impact.lon) * DEG);
    const vn = horizontalSpeed(v0, elev) * Math.cos(bearing(launch.lat, launch.lon, impact.lat, impact.lon) * DEG);
    const vu = v0 * Math.sin(elev) - G * t;
    points.push({
      t,
      lat,
      lon,
      alt: i === 0 || i === n - 1 ? 0 : alt,
      ve,
      vn,
      vu: i === n - 1 ? Math.min(vu, 0) : vu,
    });
  }
  points[0].alt = 0;
  points[0].vu = v0 * Math.sin(elev);
  points[n - 1].alt = 0;
  points[n - 1].lat = impact.lat;
  points[n - 1].lon = impact.lon;
  points[n - 1].vu = Math.min(points[n - 1].vu, -20);
  return points;
}

function horizontalSpeed(v0, elev) {
  return v0 * Math.cos(elev);
}

function randomGulfPoint(rng) {
  return {
    lat: lerp(GULF.latMin, GULF.latMax, rng()),
    lon: lerp(GULF.lonMin, GULF.lonMax, rng()),
  };
}

function inGulf(lat, lon) {
  return lat >= GULF.latMin && lat <= GULF.latMax && lon >= GULF.lonMin && lon <= GULF.lonMax;
}

function interpolateGc(lat1, lon1, lat2, lon2, f) {
  const p1 = latLonToVec(lat1, lon1);
  const p2 = latLonToVec(lat2, lon2);
  const dot = clamp(p1[0] * p2[0] + p1[1] * p2[1] + p1[2] * p2[2], -1, 1);
  const omega = Math.acos(dot);
  if (omega < 1e-9) return { lat: lat1, lon: lon1 };
  const s1 = Math.sin((1 - f) * omega) / Math.sin(omega);
  const s2 = Math.sin(f * omega) / Math.sin(omega);
  return vecToLatLon([
    s1 * p1[0] + s2 * p2[0],
    s1 * p1[1] + s2 * p2[1],
    s1 * p1[2] + s2 * p2[2],
  ]);
}

function destination(lat, lon, bearingDeg, distM) {
  const ang = distM / R_EARTH;
  const br = bearingDeg * DEG;
  const lat1 = lat * DEG;
  const lon1 = lon * DEG;
  const lat2 = Math.asin(
    Math.sin(lat1) * Math.cos(ang) + Math.cos(lat1) * Math.sin(ang) * Math.cos(br),
  );
  const lon2 =
    lon1 +
    Math.atan2(
      Math.sin(br) * Math.sin(ang) * Math.cos(lat1),
      Math.cos(ang) - Math.sin(lat1) * Math.sin(lat2),
    );
  return { lat: lat2 / DEG, lon: lon2 / DEG };
}

function bearing(lat1, lon1, lat2, lon2) {
  const φ1 = lat1 * DEG;
  const φ2 = lat2 * DEG;
  const Δλ = (lon2 - lon1) * DEG;
  const y = Math.sin(Δλ) * Math.cos(φ2);
  const x = Math.cos(φ1) * Math.sin(φ2) - Math.sin(φ1) * Math.cos(φ2) * Math.cos(Δλ);
  return (Math.atan2(y, x) / DEG + 360) % 360;
}

function latLonToVec(lat, lon) {
  const φ = lat * DEG;
  const λ = lon * DEG;
  return [Math.cos(φ) * Math.cos(λ), Math.cos(φ) * Math.sin(λ), Math.sin(φ)];
}

function vecToLatLon([x, y, z]) {
  return {
    lat: Math.atan2(z, Math.hypot(x, y)) / DEG,
    lon: Math.atan2(y, x) / DEG,
  };
}

function lerp(a, b, t) {
  return a + (b - a) * t;
}

function clamp(v, lo, hi) {
  return Math.min(hi, Math.max(lo, v));
}

function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a += 0x6d2b79f5;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
