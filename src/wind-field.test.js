import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULT_WIND_ALPHA, MAX_WIND_PARTICLES, setSurfaceWindAlpha, surfaceWindAlpha } from "./wind-layer.js";
import {
  GFS_SURFACE_ENDPOINT,
  advectInto,
  fieldFromSamples,
  gfsSurfaceBatchUrl,
  gridPointCount,
  gridSpec,
  meteoToUv,
  sampleWindInto,
  speedRgb,
  wrapLon,
} from "./wind-field.js";

test("particle simulation is capped and see-through by default", () => {
  assert.equal(MAX_WIND_PARTICLES, 1000);
  assert.ok(MAX_WIND_PARTICLES <= 1500);
  assert.ok(DEFAULT_WIND_ALPHA <= 0.5);
  assert.ok(DEFAULT_WIND_ALPHA >= 0.2);
  assert.equal(setSurfaceWindAlpha(0.25), 0.25);
  assert.equal(surfaceWindAlpha(), 0.25);
  assert.equal(setSurfaceWindAlpha(4), 1);
  setSurfaceWindAlpha(DEFAULT_WIND_ALPHA);
});

test("west wind blows east and north wind blows south", () => {
  const [uw, vw] = meteoToUv(10, 270);
  assert.ok(Math.abs(uw - 10) < 1e-9);
  assert.ok(Math.abs(vw) < 1e-9);
  const [un, vn] = meteoToUv(8, 0);
  assert.ok(Math.abs(un) < 1e-9);
  assert.ok(Math.abs(vn + 8) < 1e-9);
});

test("global grid is a 10 by 15 degree GFS lattice", () => {
  const spec = gridSpec();
  assert.equal(spec.lats[0], -80);
  assert.equal(spec.lats.at(-1), 80);
  assert.equal(spec.lons[0], -180);
  assert.equal(spec.lons.at(-1), 165);
  assert.equal(spec.lats.length, 17);
  assert.equal(spec.lons.length, 24);
  assert.equal(spec.lons.length * spec.lonStep, 360);
  assert.equal(gridPointCount(), 408);
});

test("samples a grid node and bilinearly blends neighbors", () => {
  const field = fieldFromSamples(
    { lats: [0, 10], lons: [-10, 0], step: 10 },
    [
      { speed: 10, direction: 270, time: "2026-10-07T00:00" },
      { speed: 0, direction: 0, time: "2026-10-07T00:00" },
      { speed: 0, direction: 0 },
      { speed: 0, direction: 0 },
    ],
  );
  const out = { u: 0, v: 0, speed: 0 };
  sampleWindInto(field, 0, -10, out);
  assert.ok(Math.abs(out.u - 10) < 1e-6);
  assert.ok(Math.abs(out.v) < 1e-6);
  sampleWindInto(field, 5, -5, out);
  assert.ok(Math.abs(out.u - 2.5) < 1e-6);
  assert.equal(field.time, "2026-10-07T00:00");
  assert.equal(field.endpoint, GFS_SURFACE_ENDPOINT);
});

test("longitude samples wrap across the antimeridian", () => {
  const field = fieldFromSamples(
    { lats: [0, 10], lons: [-180, 0], step: 180 },
    [
      { speed: 0, direction: 0 },
      { speed: 10, direction: 270 },
      { speed: 0, direction: 0 },
      { speed: 0, direction: 0 },
    ],
  );
  const out = { u: 0, v: 0, speed: 0 };
  sampleWindInto(field, 0, 90, out);
  assert.ok(Math.abs(out.u - 5) < 1e-6, `u=${out.u}`);
  sampleWindInto(field, 0, -190, out);
  sampleWindInto(field, 0, wrapLon(-190), out);
  assert.equal(wrapLon(-190), 170);
  assert.equal(wrapLon(180), -180);
  assert.equal(wrapLon(-180), -180);
});

test("eastward wind increases longitude", () => {
  const out = { lat: 0, lon: 0 };
  advectInto(0, 0, 10, 0, 3600, out);
  const expected = (10 * 3600 / 6378137) * (180 / Math.PI);
  assert.ok(Math.abs(out.lon - expected) < 1e-6);
  assert.ok(Math.abs(out.lat) < 1e-9);
  advectInto(0, 0, 0, -10, 3600, out);
  assert.ok(out.lat < 0);
});

test("speed colors stay in range and shift from blue toward red", () => {
  const calm = speedRgb(0);
  const fast = speedRgb(40);
  assert.ok(calm[2] > calm[0]);
  assert.ok(fast[0] > fast[2]);
});

test("batch URL asks Open-Meteo GFS for 10 m wind", () => {
  const url = gfsSurfaceBatchUrl([{ lat: 28, lon: -90.25 }]);
  assert.match(url, /^https:\/\/api\.open-meteo\.com\/v1\/gfs\?/);
  assert.match(url, /current=wind_speed_10m,wind_direction_10m/);
  assert.match(url, /wind_speed_unit=ms/);
  assert.match(url, /latitude=28\.0000/);
  assert.match(url, /longitude=-90\.2500/);
});
