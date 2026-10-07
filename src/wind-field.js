/**
 * Current GFS 10 m winds for the globe overlay.
 *
 * The trajectory simulator already calls Open-Meteo GFS for historical
 * soundings (src-tauri/src/wind.rs). This module asks the same service for a
 * global spatial grid of current `wind_speed_10m` and `wind_direction_10m`.
 * Direction is meteorological (the direction the wind comes FROM) and is
 * converted to east/north components the same way as `meteo_to_enu`.
 */

export const GFS_SURFACE_ENDPOINT = "https://api.open-meteo.com/v1/gfs";
export const GFS_GRID_STEP_DEG = 5;
const BATCH_SIZE = 200;
const FETCH_CONCURRENCY = 2;
const CACHE_MS = 10 * 60 * 1000;

const SPEED_STOPS = [
  [0, [68, 90, 120]],
  [4, [52, 122, 186]],
  [8, [36, 176, 186]],
  [12, [70, 186, 96]],
  [18, [214, 196, 62]],
  [25, [224, 132, 48]],
  [35, [204, 64, 58]],
  [50, [150, 48, 120]],
];

const EARTH_RADIUS_M = 6378137;

let cachedField = null;
let cachedAt = 0;

export function gridSpec(step = GFS_GRID_STEP_DEG) {
  const lats = [];
  for (let lat = -80; lat <= 80; lat += step) lats.push(lat);
  const lons = [];
  for (let lon = -180; lon < 180; lon += step) lons.push(lon);
  return { lats, lons, step };
}

export function gridPointCount(step = GFS_GRID_STEP_DEG) {
  const spec = gridSpec(step);
  return spec.lats.length * spec.lons.length;
}

/** Meteorological FROM direction → eastward (u) and northward (v), m/s. */
export function meteoToUv(speedMps, fromDeg) {
  const speed = Number.isFinite(speedMps) ? Math.max(0, speedMps) : 0;
  const rad = (Number(fromDeg) || 0) * (Math.PI / 180);
  return [-speed * Math.sin(rad), -speed * Math.cos(rad)];
}

export function wrapLon(lon) {
  const x = ((lon + 180) % 360 + 360) % 360 - 180;
  return Object.is(x, -0) ? 0 : x;
}

export function fieldFromSamples(spec, samples) {
  const ny = spec.lats.length;
  const nx = spec.lons.length;
  if (ny < 2 || nx < 2) throw new Error("wind grid is too small");
  if (samples.length !== nx * ny) {
    throw new Error(`wind grid has ${samples.length} samples, expected ${nx * ny}`);
  }
  const dlon = spec.lons[1] - spec.lons[0];
  const dlat = spec.lats[1] - spec.lats[0];
  if (!(dlat > 0) || !(dlon > 0)) throw new Error("wind grid axes must increase");
  const global = Math.abs(nx * dlon - 360) < 1e-3;
  const u = new Float32Array(nx * ny);
  const v = new Float32Array(nx * ny);
  let time = "";
  let missing = 0;
  for (let i = 0; i < samples.length; i++) {
    const sample = samples[i];
    if (!sample || sample.speed == null || sample.direction == null || Number.isNaN(sample.speed)) {
      missing += 1;
      continue;
    }
    const [eu, nv] = meteoToUv(sample.speed, sample.direction);
    u[i] = eu;
    v[i] = nv;
    if (!time && sample.time) time = sample.time;
  }
  if (missing > samples.length * 0.25) {
    throw new Error("GFS response was missing most of the 10 m wind grid");
  }
  return {
    lats: Float64Array.from(spec.lats),
    lons: Float64Array.from(spec.lons),
    u,
    v,
    nx,
    ny,
    step: spec.step,
    global,
    time,
    source: "Open-Meteo GFS",
    level: "10 m",
    endpoint: GFS_SURFACE_ENDPOINT,
  };
}

export function sampleWindInto(grid, lat, lon, out) {
  const { lats, lons, u, v, nx, ny } = grid;
  const dlat = lats[1] - lats[0];
  const dlon = lons[1] - lons[0];
  let fy = (lat - lats[0]) / dlat;
  if (fy < 0) fy = 0;
  else if (fy > ny - 1) fy = ny - 1;
  const j0 = Math.min(ny - 1, Math.floor(fy));
  const j1 = Math.min(ny - 1, j0 + 1);
  const ty = fy - j0;
  let fx = (wrapLon(lon) - lons[0]) / dlon;
  if (grid.global) fx = ((fx % nx) + nx) % nx;
  else if (fx < 0) fx = 0;
  else if (fx > nx - 1) fx = nx - 1;
  const i0 = Math.min(nx - 1, Math.floor(fx));
  const tx = fx - i0;
  const i1 = i0 + 1 >= nx ? 0 : i0 + 1;
  const sy = 1 - ty;
  const sx = 1 - tx;
  const i00 = j0 * nx + i0;
  const i10 = j0 * nx + i1;
  const i01 = j1 * nx + i0;
  const i11 = j1 * nx + i1;
  out.u = u[i00] * sx * sy + u[i10] * tx * sy + u[i01] * sx * ty + u[i11] * tx * ty;
  out.v = v[i00] * sx * sy + v[i10] * tx * sy + v[i01] * sx * ty + v[i11] * tx * ty;
  out.speed = Math.hypot(out.u, out.v);
  return out;
}

/** Integrate a horizontal wind. `dtSeconds` is model time, not frame time. */
export function advectInto(lat, lon, u, v, dtSeconds, out) {
  const cos = Math.max(0.15, Math.cos(lat * (Math.PI / 180)));
  const deg = (180 / Math.PI) / EARTH_RADIUS_M;
  let nlat = lat + v * dtSeconds * deg;
  let nlon = lon + (u * dtSeconds * deg) / cos;
  if (nlat > 80) nlat = 80;
  else if (nlat < -80) nlat = -80;
  out.lat = nlat;
  out.lon = wrapLon(nlon);
  return out;
}

export function speedRgb(mps) {
  const speed = Math.max(0, Number(mps) || 0);
  const last = SPEED_STOPS.length - 1;
  if (speed <= SPEED_STOPS[0][0]) return SPEED_STOPS[0][1];
  for (let i = 1; i <= last; i++) {
    if (speed <= SPEED_STOPS[i][0] || i === last) {
      const [s0, c0] = SPEED_STOPS[i - 1];
      const [s1, c1] = SPEED_STOPS[i];
      const t = s1 === s0 ? 1 : Math.min(1, (speed - s0) / (s1 - s0));
      return [
        Math.round(c0[0] + (c1[0] - c0[0]) * t),
        Math.round(c0[1] + (c1[1] - c0[1]) * t),
        Math.round(c0[2] + (c1[2] - c0[2]) * t),
      ];
    }
  }
  return SPEED_STOPS[last][1];
}

export function speedRampCss() {
  const last = SPEED_STOPS[SPEED_STOPS.length - 1][0];
  const parts = SPEED_STOPS.map(([speed, rgb]) => {
    const pct = (100 * speed) / last;
    return `rgb(${rgb[0]}, ${rgb[1]}, ${rgb[2]}) ${pct}%`;
  });
  return `linear-gradient(90deg, ${parts.join(", ")})`;
}

export function gfsSurfaceBatchUrl(points) {
  const lats = points.map((p) => Number(p.lat).toFixed(4)).join(",");
  const lons = points.map((p) => Number(p.lon).toFixed(4)).join(",");
  return `${GFS_SURFACE_ENDPOINT}?latitude=${lats}&longitude=${lons}&current=wind_speed_10m,wind_direction_10m&wind_speed_unit=ms&timezone=GMT`;
}

function sleep(ms, signal) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(abortError(signal));
    };
    if (signal?.aborted) {
      clearTimeout(timer);
      reject(abortError(signal));
      return;
    }
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function abortError(signal) {
  return signal?.reason instanceof Error
    ? signal.reason
    : new DOMException("Aborted", "AbortError");
}

async function fetchBatch(points, signal, attempt = 0) {
  const response = await fetch(gfsSurfaceBatchUrl(points), { signal });
  if (response.status === 429 && attempt < 5) {
    await sleep(700 * 2 ** attempt, signal);
    return fetchBatch(points, signal, attempt + 1);
  }
  if (!response.ok) {
    let reason = "";
    try {
      reason = (await response.json())?.reason || "";
    } catch {
      /* body was not JSON */
    }
    throw new Error(reason || `GFS wind request failed (${response.status})`);
  }
  const json = await response.json();
  if (!Array.isArray(json)) {
    throw new Error("GFS wind response was not a location grid");
  }
  if (json.length !== points.length) {
    throw new Error("GFS wind batch size mismatch");
  }
  return json.map((row) => {
    const current = row?.current;
    const speed = current?.wind_speed_10m;
    const direction = current?.wind_direction_10m;
    if (speed == null || direction == null) {
      return { speed: null, direction: null, time: current?.time || "" };
    }
    return {
      speed: Number(speed),
      direction: Number(direction),
      time: current.time || "",
    };
  });
}

export async function fetchGfsSurfaceField(signal) {
  const spec = gridSpec();
  const points = [];
  for (const lat of spec.lats) {
    for (const lon of spec.lons) points.push({ lat, lon });
  }
  const samples = new Array(points.length);
  const offsets = [];
  for (let offset = 0; offset < points.length; offset += BATCH_SIZE) offsets.push(offset);
  let cursor = 0;
  async function worker() {
    while (cursor < offsets.length) {
      const offset = offsets[cursor++];
      const slice = points.slice(offset, offset + BATCH_SIZE);
      const rows = await fetchBatch(slice, signal);
      for (let i = 0; i < rows.length; i++) samples[offset + i] = rows[i];
    }
  }
  const workers = [];
  for (let i = 0; i < FETCH_CONCURRENCY; i++) workers.push(worker());
  await Promise.all(workers);
  return fieldFromSamples(spec, samples);
}

export async function loadGfsSurfaceField(signal) {
  if (cachedField && Date.now() - cachedAt < CACHE_MS) return cachedField;
  const field = await fetchGfsSurfaceField(signal);
  cachedField = field;
  cachedAt = Date.now();
  return field;
}

/** Drop the cached grid. The animation layer also drops its own particles. */
export function clearGfsSurfaceCache() {
  cachedField = null;
  cachedAt = 0;
}
