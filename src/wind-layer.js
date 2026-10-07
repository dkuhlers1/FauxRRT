/**
 * Animated surface-wind particles on a canvas above the Cesium globe.
 *
 * The particle count is capped. Positions are updated on requestAnimationFrame
 * and drawn in screen space, so the Cesium scene stays on its on-demand
 * render loop. Turning the layer off aborts the fetch, cancels the frame
 * loop, and removes the canvas.
 */

import {
  advectInto,
  loadGfsSurfaceField,
  sampleWindInto,
  speedRampCss,
  speedRgb,
  wrapLon,
} from "./wind-field.js";

export const MAX_WIND_PARTICLES = 1000;
const SPEED_BINS = 24;
const BIN_MAX_MPS = 42;
const POLE_LIMIT = 79.5;

let generation = 0;
let abortCtrl = null;
let raf = 0;
let running = false;
let canvas = null;
let legend = null;
let particles = null;

export function setSurfaceWindEnabled(viewer, enabled, onStatus) {
  generation += 1;
  const gen = generation;
  stopRuntime();
  if (!enabled) {
    onStatus?.({ phase: "off" });
    return Promise.resolve({ ok: true });
  }
  if (!viewer) {
    onStatus?.({ phase: "error", message: "Globe is not ready" });
    return Promise.resolve({ ok: false, message: "Globe is not ready" });
  }
  abortCtrl = new AbortController();
  const signal = abortCtrl.signal;
  onStatus?.({ phase: "loading" });
  return loadGfsSurfaceField(signal).then((field) => {
    if (gen !== generation || signal.aborted) return { ok: false, aborted: true };
    startLayer(viewer, field, gen);
    onStatus?.({ phase: "ready", field });
    return { ok: true, field };
  }).catch((err) => {
    if (gen !== generation || err?.name === "AbortError") return { ok: false, aborted: true };
    stopRuntime();
    const message = err?.message || String(err);
    onStatus?.({ phase: "error", message });
    return { ok: false, message };
  });
}

function stopRuntime() {
  running = false;
  if (raf) cancelAnimationFrame(raf);
  raf = 0;
  abortCtrl?.abort();
  abortCtrl = null;
  canvas?.remove();
  legend?.remove();
  canvas = null;
  legend = null;
  particles = null;
}

function startLayer(viewer, field, gen) {
  const Cesium = window.Cesium;
  const project = Cesium?.SceneTransforms?.worldToWindowCoordinates
    || Cesium?.SceneTransforms?.wgs84ToWindowCoordinates;
  if (!project) throw new Error("Cesium cannot project wind particles");

  const hostCanvas = viewer.scene.canvas;
  const parent = hostCanvas.parentElement || viewer.container;
  if (getComputedStyle(parent).position === "static") parent.style.position = "relative";
  canvas = document.createElement("canvas");
  canvas.className = "surface-wind-canvas";
  canvas.dataset.windLayer = "on";
  canvas.setAttribute("aria-hidden", "true");
  canvas.style.position = "absolute";
  canvas.style.inset = "0";
  canvas.style.width = "100%";
  canvas.style.height = "100%";
  canvas.style.pointerEvents = "none";
  canvas.style.zIndex = "1";
  parent.appendChild(canvas);
  legend = buildLegend(field);
  parent.appendChild(legend);

  const ctx = canvas.getContext("2d", { alpha: true });
  const lat = new Float32Array(MAX_WIND_PARTICLES);
  const lon = new Float32Array(MAX_WIND_PARTICLES);
  const age = new Float32Array(MAX_WIND_PARTICLES);
  const life = new Float32Array(MAX_WIND_PARTICLES);
  const buckets = Array.from({ length: SPEED_BINS }, () => []);
  const binColors = Array.from({ length: SPEED_BINS }, (_, bin) => {
    const speed = ((bin + 0.5) / SPEED_BINS) * BIN_MAX_MPS;
    const [r, g, b] = speedRgb(speed);
    return `rgb(${r}, ${g}, ${b})`;
  });
  const wind = { u: 0, v: 0, speed: 0 };
  const here = { lat: 0, lon: 0 };
  const tail = { lat: 0, lon: 0 };
  const headXy = { x: 0, y: 0 };
  const tailXy = { x: 0, y: 0 };
  const scratch3 = new Cesium.Cartesian3();
  const scratch2 = new Cesium.Cartesian2();
  const carto = new Cesium.Cartographic();
  let last = 0;

  particles = { lat, lon, age, life };
  running = true;

  const spawn = (index, center, halfLat, halfLon) => {
    const lat0 = center ? center.lat : 28;
    const lon0 = center ? center.lon : -90;
    lat[index] = clamp(lat0 + (Math.random() - 0.5) * 2 * halfLat, -80, 80);
    lon[index] = wrapLon(lon0 + (Math.random() - 0.5) * 2 * halfLon);
    age[index] = Math.random() * 2;
    life[index] = 4 + Math.random() * 6;
  };

  const frame = (now) => {
    if (!running || gen !== generation) return;
    const host = viewer.scene.canvas;
    const cssW = host.clientWidth || parent.clientWidth;
    const cssH = host.clientHeight || parent.clientHeight;
    if (cssW < 2 || cssH < 2) {
      raf = requestAnimationFrame(frame);
      return;
    }
    const dpr = Math.min(window.devicePixelRatio || 1, 1.25);
    const bufW = Math.floor(cssW * dpr);
    const bufH = Math.floor(cssH * dpr);
    if (canvas.width !== bufW || canvas.height !== bufH) {
      canvas.width = bufW;
      canvas.height = bufH;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);

    const dt = last ? Math.min(0.05, (now - last) / 1000) : 0.016;
    last = now;
    const mpp = metersPerPixel(viewer, cssH);
    const timeScale = 2.4 * mpp;
    const center = viewCenter(viewer, Cesium, scratch2, carto);
    const halfLat = Math.min(70, Math.max(1.2, (mpp * cssH * 0.48) / 111320));
    const cos = Math.max(0.25, Math.cos(((center?.lat ?? 0) * Math.PI) / 180));
    const halfLon = Math.min(160, halfLat / cos);

    for (let b = 0; b < buckets.length; b++) buckets[b].length = 0;

    for (let i = 0; i < MAX_WIND_PARTICLES; i++) {
      sampleWindInto(field, lat[i], lon[i], wind);
      age[i] += dt;
      const calm = !(wind.speed >= 0.35) || !Number.isFinite(wind.u);
      const expired = age[i] > life[i] || (calm && age[i] > 0.7);
      const polar = lat[i] <= -POLE_LIMIT || lat[i] >= POLE_LIMIT;
      if (expired || polar || !Number.isFinite(lat[i])) {
        spawn(i, center, halfLat, halfLon);
        continue;
      }
      advectInto(lat[i], lon[i], wind.u, wind.v, dt * timeScale, here);
      lat[i] = here.lat;
      lon[i] = here.lon;
      if (!projectFront(viewer, Cesium, project, here.lat, here.lon, scratch3, scratch2, headXy)) {
        if (age[i] > 0.45) spawn(i, center, halfLat, halfLon);
        continue;
      }
      if (headXy.x < -40 || headXy.y < -40 || headXy.x > cssW + 40 || headXy.y > cssH + 40) {
        if (age[i] > 0.45) spawn(i, center, halfLat, halfLon);
        continue;
      }
      const streak = streakSeconds(wind.speed, mpp);
      advectInto(here.lat, here.lon, wind.u, wind.v, -streak, tail);
      if (!projectFront(viewer, Cesium, project, tail.lat, tail.lon, scratch3, scratch2, tailXy)) continue;
      const bin = Math.min(SPEED_BINS - 1, Math.floor((Math.min(BIN_MAX_MPS, wind.speed) / BIN_MAX_MPS) * SPEED_BINS));
      buckets[bin].push(headXy.x, headXy.y, tailXy.x, tailXy.y);
    }

    ctx.lineWidth = 1.35;
    ctx.lineCap = "round";
    ctx.globalAlpha = 0.9;
    for (let b = 0; b < buckets.length; b++) {
      const bucket = buckets[b];
      if (!bucket.length) continue;
      ctx.strokeStyle = binColors[b];
      ctx.fillStyle = binColors[b];
      ctx.beginPath();
      for (let k = 0; k < bucket.length; k += 4) {
        ctx.moveTo(bucket[k + 2], bucket[k + 3]);
        ctx.lineTo(bucket[k], bucket[k + 1]);
      }
      ctx.stroke();
      for (let k = 0; k < bucket.length; k += 4) {
        ctx.fillRect(bucket[k] - 1.15, bucket[k + 1] - 1.15, 2.3, 2.3);
      }
    }

    if (running && gen === generation) raf = requestAnimationFrame(frame);
  };

  const seedCenter = viewCenter(viewer, Cesium, scratch2, carto);
  const seedH = viewer.scene.canvas.clientHeight || 800;
  const seedMpp = metersPerPixel(viewer, seedH);
  const seedHalfLat = Math.min(70, Math.max(1.2, (seedMpp * seedH * 0.48) / 111320));
  const seedCos = Math.max(0.25, Math.cos(((seedCenter?.lat ?? 0) * Math.PI) / 180));
  for (let i = 0; i < MAX_WIND_PARTICLES; i++) {
    spawn(i, seedCenter, seedHalfLat, Math.min(160, seedHalfLat / seedCos));
  }
  viewer.scene.requestRender();
  raf = requestAnimationFrame(frame);
}

function streakSeconds(speed, mpp) {
  const px = Math.min(42, Math.max(5, speed * 1.6));
  return (px * mpp) / Math.max(speed, 0.5);
}

function metersPerPixel(viewer, cssH) {
  const height = viewer.camera.positionCartographic?.height ?? 2e7;
  const frustum = viewer.camera.frustum;
  const h = Math.max(1, cssH);
  if (frustum?.fovy) return (2 * Math.max(height, 1) * Math.tan(frustum.fovy * 0.5)) / h;
  if (frustum?.width) return frustum.width / Math.max(1, viewer.scene.canvas.clientWidth || h);
  return Math.max(height, 1) / h;
}

function viewCenter(viewer, Cesium, scratch2, carto) {
  const canvas = viewer.scene.canvas;
  scratch2.x = (canvas.clientWidth || 0) * 0.5;
  scratch2.y = (canvas.clientHeight || 0) * 0.5;
  const cart = viewer.camera.pickEllipsoid(scratch2, viewer.scene.globe.ellipsoid);
  if (!cart) {
    const c = viewer.camera.positionCartographic;
    if (!c) return null;
    return {
      lat: Cesium.Math.toDegrees(c.latitude),
      lon: Cesium.Math.toDegrees(c.longitude),
    };
  }
  const geo = Cesium.Cartographic.fromCartesian(cart, viewer.scene.globe.ellipsoid, carto);
  return {
    lat: Cesium.Math.toDegrees(geo.latitude),
    lon: Cesium.Math.toDegrees(geo.longitude),
  };
}

function projectFront(viewer, Cesium, project, lat, lon, scratch3, scratch2, out) {
  Cesium.Cartesian3.fromDegrees(lon, lat, 0, Cesium.Ellipsoid.WGS84, scratch3);
  if (viewer.scene.mode === Cesium.SceneMode.SCENE3D) {
    const cam = viewer.camera.positionWC;
    const facing = scratch3.x * cam.x + scratch3.y * cam.y + scratch3.z * cam.z;
    const r2 = scratch3.x * scratch3.x + scratch3.y * scratch3.y + scratch3.z * scratch3.z;
    if (facing <= r2) return false;
  }
  const win = project(viewer.scene, scratch3, scratch2);
  if (!win) return false;
  out.x = win.x;
  out.y = win.y;
  return true;
}

function buildLegend(field) {
  const el = document.createElement("aside");
  el.className = "wind-legend";
  el.dataset.windLegend = "on";
  const title = document.createElement("div");
  title.className = "wind-legend-title";
  title.textContent = "Surface wind";
  const meta = document.createElement("div");
  meta.className = "wind-legend-meta";
  const when = (field.time || "current").replace("T", " ");
  meta.textContent = `GFS 10 m · ${when} UTC`;
  const bar = document.createElement("div");
  bar.className = "wind-legend-bar";
  bar.style.background = speedRampCss();
  const scale = document.createElement("div");
  scale.className = "wind-legend-scale";
  for (const label of ["0", "10", "20", "40 m/s"]) {
    const tick = document.createElement("span");
    tick.textContent = label;
    scale.appendChild(tick);
  }
  const src = document.createElement("div");
  src.className = "wind-legend-meta";
  src.textContent = "Open-Meteo GFS · directional speed";
  el.append(title, meta, bar, scale, src);
  return el;
}

function clamp(value, min, max) {
  return Math.max(min, Math.min(max, value));
}
