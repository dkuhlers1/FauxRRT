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
/** 17×24 = 408 samples. Fine enough for globe-scale flow, small enough for one Open-Meteo minute. */
export const GFS_GRID_LAT_STEP_DEG = 10;
export const GFS_GRID_LON_STEP_DEG = 15;
const BATCH_SIZE = 80;
const FETCH_CONCURRENCY = 1;
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
let inflight = null;

export function gridSpec(latStep = GFS_GRID_LAT_STEP_DEG, lonStep = GFS_GRID_LON_STEP_DEG) {
  const lats = [];
  for (let lat = -80; lat <= 80; lat += latStep) lats.push(lat);
  const lons = [];
  for (let lon = -180; lon < 180; lon += lonStep) lons.push(lon);
  return { lats, lons, latStep, lonStep, step: latStep };
}

export function gridPointCount(latStep = GFS_GRID_LAT_STEP_DEG, lonStep = GFS_GRID_LON_STEP_DEG) {
  const spec = gridSpec(latStep, lonStep);
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
    kind: "grid",
    lats: Float64Array.from(spec.lats),
    lons: Float64Array.from(spec.lons),
    u,
    v,
    nx,
    ny,
    step: spec.step,
    global,
    strict: false,
    time,
    source: "Open-Meteo GFS",
    level: "10 m",
    endpoint: GFS_SURFACE_ENDPOINT,
    legend: {
      title: "Surface wind",
      meta: `GFS 10 m · ${(time || "current").replace("T", " ")} UTC`,
      source: "Open-Meteo GFS · directional speed",
    },
  };
}

/**
 * Globe streaks for a mission-wind Surface grid already stored in east/north
 * components. This does not fetch GFS again.
 */
export function surfaceFieldFromComponents({ lats, lons, east_mps: east, north_mps: north, time, source }) {
  const latArr = Float64Array.from(lats || []);
  const lonArr = Float64Array.from(lons || []);
  const ny = latArr.length;
  const nx = lonArr.length;
  if (ny < 2 || nx < 2 || !east || !north || east.length !== nx * ny || north.length !== east.length) {
    throw new Error("surface wind grid is incomplete");
  }
  const dlat = latArr[1] - latArr[0];
  const dlon = lonArr[1] - lonArr[0];
  if (!(dlat > 0) || !(dlon > 0)) throw new Error("wind grid axes must increase");
  const global = Math.abs(nx * dlon - 360) < 1e-3;
  const when = time || "";
  return {
    kind: "grid",
    lats: latArr,
    lons: lonArr,
    u: Float32Array.from(east),
    v: Float32Array.from(north),
    nx,
    ny,
    step: dlat,
    global,
    strict: false,
    time: when,
    source: source || "Open-Meteo GFS",
    level: "10 m",
    endpoint: GFS_SURFACE_ENDPOINT,
    legend: {
      title: "Surface wind",
      meta: `GFS 10 m · ${(when || "current").replace("T", " ")} UTC`,
      source: "Open-Meteo GFS · directional speed",
    },
  };
}

/** True only when Surface was selected without the grid the mission already holds. */
export function surfaceAnimationNeedsFetch(request) {
  if (!request || request.type !== "surface") return false;
  const nLat = request.lats?.length || 0;
  const nLon = request.lons?.length || 0;
  const n = request.east_mps?.length || 0;
  return !(nLat >= 2 && nLon >= 2 && n === nLat * nLon && request.north_mps?.length === n);
}

export function constantWindField(speedMps, fromDeg) {
  const speed = Number(speedMps);
  const from = Number(fromDeg);
  const [u, v] = meteoToUv(Number.isFinite(speed) ? speed : 0, Number.isFinite(from) ? from : 0);
  const shownSpeed = Number.isFinite(speed) ? speed : 0;
  const shownFrom = Number.isFinite(from) ? from : 0;
  return {
    kind: "constant",
    global: true,
    u,
    v,
    speed: Math.hypot(u, v),
    legend: {
      title: "Constant wind",
      meta: `${trimNum(shownSpeed)} m/s from ${trimNum(shownFrom)}°`,
      source: "Same vector everywhere",
    },
  };
}

/** Minimum pad around a profile or corridor, in degrees. A point sounding stays local. */
export const HISTORICAL_PAD_DEG = 0.6;

function trimNum(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return "0";
  return String(Math.round(n * 10) / 10);
}

function miss(out) {
  out.u = NaN;
  out.v = NaN;
  out.speed = NaN;
  out.miss = true;
  return out;
}

/** Short-way longitude offsets from `ref`, in degrees. */
function lonOffset(lon, ref) {
  let d = lon - ref;
  if (d > 180) d -= 360;
  if (d < -180) d += 360;
  return d;
}

export function boundsFromStations(stations, padDeg = HISTORICAL_PAD_DEG) {
  if (!stations?.length) return null;
  const ref = stations[0].lon;
  let minD = 0;
  let maxD = 0;
  let minLat = stations[0].lat;
  let maxLat = stations[0].lat;
  for (const station of stations) {
    const d = lonOffset(station.lon, ref);
    if (d < minD) minD = d;
    if (d > maxD) maxD = d;
    if (station.lat < minLat) minLat = station.lat;
    if (station.lat > maxLat) maxLat = station.lat;
  }
  const padLat = Math.max(padDeg, (maxLat - minLat) * 0.25);
  const padLon = Math.max(padDeg, (maxD - minD) * 0.25);
  return {
    ref,
    minD: minD - padLon,
    maxD: maxD + padLon,
    minLat: Math.max(-80, minLat - padLat),
    maxLat: Math.min(80, maxLat + padLat),
    spanLat: maxLat - minLat,
    spanLon: maxD - minD,
  };
}

export function boundsContain(bounds, lat, lon) {
  if (!bounds) return false;
  if (lat < bounds.minLat || lat > bounds.maxLat) return false;
  const d = lonOffset(lon, bounds.ref);
  return d >= bounds.minD && d <= bounds.maxD;
}

function haversineM(lat1, lon1, lat2, lon2) {
  const r = 6_371_000;
  const p1 = (lat1 * Math.PI) / 180;
  const p2 = (lat2 * Math.PI) / 180;
  const dp = ((lat2 - lat1) * Math.PI) / 180;
  const dl = ((lon2 - lon1) * Math.PI) / 180;
  const a = Math.sin(dp / 2) ** 2 + Math.cos(p1) * Math.cos(p2) * Math.sin(dl / 2) ** 2;
  return 2 * r * Math.asin(Math.min(1, Math.sqrt(a)));
}

/** Inverse-distance blend of surface station components. Same weights as trajectory sampling. */
export function sampleStationsInto(stations, lat, lon, out) {
  if (!stations?.length) return miss(out);
  if (stations.length === 1) {
    out.u = stations[0].east;
    out.v = stations[0].north;
    out.speed = Math.hypot(out.u, out.v);
    out.miss = false;
    return out;
  }
  let wsum = 0;
  let east = 0;
  let north = 0;
  for (const station of stations) {
    const dist = Math.max(1000, haversineM(lat, lon, station.lat, station.lon));
    const w = 1 / (dist * dist);
    east += w * station.east;
    north += w * station.north;
    wsum += w;
  }
  out.u = east / wsum;
  out.v = north / wsum;
  out.speed = Math.hypot(out.u, out.v);
  out.miss = false;
  return out;
}

function regionSpanDeg(region) {
  return Math.max(region.bounds?.spanLat || 0, region.bounds?.spanLon || 0);
}

function historicalLegend(spec, regions) {
  const hour = String(spec.hour_utc ?? 0).padStart(2, "0");
  const when = `${spec.date || "historical"} ${hour}Z`;
  if (spec.loading) {
    return { title: "Historical wind", meta: `${when} UTC`, source: "Loading the sampled wind…" };
  }
  if (spec.error) {
    return { title: "Historical wind", meta: `${when} UTC`, source: String(spec.error) };
  }
  if (!regions.length) {
    return {
      title: "Historical wind",
      meta: `${when} UTC`,
      source: "No sampled corridor yet. Streaks appear where trajectories use this wind.",
    };
  }
  const local = regions.every((region) => regionSpanDeg(region) < 1);
  const where = regions.length === 1
    ? (local ? "Local sounding" : "Sampled corridor")
    : `${regions.length} sampled areas`;
  const src = spec.source ? `${spec.source} · not a global map` : "Not a global map";
  return { title: "Historical wind", meta: `${where} · ${when} UTC`, source: src };
}

export function historicalWindField(spec = {}) {
  const regions = (spec.regions || []).map((region) => {
    const stations = (region.stations || []).map((station) => ({
      lat: Number(station.lat),
      lon: Number(station.lon),
      east: Number(station.east_mps ?? station.east),
      north: Number(station.north_mps ?? station.north),
    })).filter((station) => Number.isFinite(station.lat) && Number.isFinite(station.lon) && Number.isFinite(station.east) && Number.isFinite(station.north));
    return { stations, bounds: boundsFromStations(stations) };
  }).filter((region) => region.stations.length && region.bounds);
  return {
    kind: "historical",
    global: false,
    date: spec.date || "",
    hour_utc: spec.hour_utc ?? 0,
    source: spec.source || "",
    regions,
    loading: Boolean(spec.loading),
    error: spec.error || "",
    legend: historicalLegend(spec, regions),
  };
}

export function resolveAnimationField(request, signal) {
  if (!request || request.type === "off") return Promise.resolve(null);
  if (request.type === "constant") {
    return Promise.resolve(constantWindField(request.speed_mps, request.from_deg));
  }
  if (request.type === "historical") return Promise.resolve(historicalWindField(request));
  if (request.type === "surface") {
    if (!surfaceAnimationNeedsFetch(request)) {
      return Promise.resolve(surfaceFieldFromComponents(request));
    }
    return loadGfsSurfaceField(signal);
  }
  return Promise.resolve(null);
}

export function sampleWindInto(grid, lat, lon, out) {
  if (!grid || grid.kind === "off") return miss(out);
  if (grid.kind === "constant") {
    out.u = grid.u;
    out.v = grid.v;
    out.speed = grid.speed;
    out.miss = false;
    return out;
  }
  if (grid.kind === "historical") {
    let best = null;
    let bestDist = Infinity;
    for (const region of grid.regions || []) {
      if (!boundsContain(region.bounds, lat, lon)) continue;
      for (const station of region.stations) {
        const dist = haversineM(lat, lon, station.lat, station.lon);
        if (dist < bestDist) {
          bestDist = dist;
          best = region;
        }
      }
    }
    if (!best) return miss(out);
    return sampleStationsInto(best.stations, lat, lon, out);
  }
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
  out.miss = false;
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
  if (!response.ok) {
    let reason = "";
    try {
      reason = (await response.json())?.reason || "";
    } catch {
      /* body was not JSON */
    }
    const retryable = response.status === 429 || response.status >= 500 || /limit|rate|overload/i.test(reason);
    if (retryable && attempt < 1) {
      await sleep(2000, signal);
      return fetchBatch(points, signal, attempt + 1);
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
      if (cursor < offsets.length) await sleep(200, signal);
    }
  }
  const workers = [];
  for (let i = 0; i < FETCH_CONCURRENCY; i++) workers.push(worker());
  await Promise.all(workers);
  return fieldFromSamples(spec, samples);
}

export function peekGfsSurfaceField() {
  if (cachedField && Date.now() - cachedAt < CACHE_MS) return cachedField;
  return null;
}

export async function loadGfsSurfaceField(signal) {
  const cached = peekGfsSurfaceField();
  if (cached) return cached;
  if (inflight) return inflight;
  inflight = fetchGfsSurfaceField(signal)
    .then((field) => {
      cachedField = field;
      cachedAt = Date.now();
      return field;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/** Drop the cached grid. The animation layer also drops its own particles. */
export function clearGfsSurfaceCache() {
  cachedField = null;
  cachedAt = 0;
}
