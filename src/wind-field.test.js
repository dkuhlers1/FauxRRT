import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULT_WIND_ALPHA, MAX_WIND_PARTICLES, setSurfaceWindAlpha, surfaceWindAlpha } from "./wind-layer.js";
import {
  GFS_SURFACE_ENDPOINT,
  advectInto,
  boundsContain,
  constantWindField,
  fieldFromSamples,
  gfsSurfaceBatchUrl,
  gridPointCount,
  gridSpec,
  historicalWindField,
  meteoToUv,
  resolveAnimationField,
  sampleWindInto,
  speedRgb,
  surfaceAnimationNeedsFetch,
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

test("constant wind is the same vector everywhere", () => {
  const field = constantWindField(10, 270);
  const out = { u: 0, v: 0, speed: 0 };
  sampleWindInto(field, 0, 0, out);
  assert.ok(Math.abs(out.u - 10) < 1e-9);
  assert.ok(Math.abs(out.v) < 1e-9);
  assert.equal(out.miss, false);
  sampleWindInto(field, -40, 150, out);
  assert.ok(Math.abs(out.u - 10) < 1e-9);
  assert.equal(field.global, true);
  assert.equal(field.legend.title, "Constant wind");
  assert.match(field.legend.source, /everywhere/);
});

test("surface selection reuses the mission grid instead of fetching GFS", async () => {
  const request = {
    type: "surface",
    time: "2026-10-07T00:00",
    source: "Open-Meteo GFS 10 m",
    lats: [0, 10],
    lons: [-10, 0],
    east_mps: [4, 0, 0, 0],
    north_mps: [0, 0, 0, 1],
  };
  assert.equal(surfaceAnimationNeedsFetch(request), false);
  assert.equal(surfaceAnimationNeedsFetch({ type: "surface" }), true);
  assert.equal(surfaceAnimationNeedsFetch({ type: "constant" }), false);
  const field = await resolveAnimationField(request);
  assert.equal(field.kind, "grid");
  assert.equal(field.global, false);
  const out = { u: 0, v: 0, speed: 0 };
  sampleWindInto(field, 0, -10, out);
  assert.ok(Math.abs(out.u - 4) < 1e-6);
  assert.ok(Math.abs(out.v) < 1e-6);
  assert.equal(field.legend.title, "Surface wind");
});

test("historical wind is sampled inside its corridor and missing outside", () => {
  const field = historicalWindField({
    date: "2024-06-01",
    hour_utc: 12,
    source: "GFS 2024-06-01 12Z",
    regions: [{
      stations: [
        { lat: 30, lon: -100, east_mps: 10, north_mps: 0 },
        { lat: 32, lon: -98, east_mps: 0, north_mps: 10 },
      ],
    }],
  });
  assert.equal(field.global, false);
  const out = { u: 0, v: 0, speed: 0, miss: false };
  sampleWindInto(field, 30, -100, out);
  assert.equal(out.miss, false);
  assert.ok(out.u > 8, `near the west station u=${out.u}`);
  sampleWindInto(field, 31, -99, out);
  assert.equal(out.miss, false);
  assert.ok(out.u > 2 && out.u < 8, `midpoint u=${out.u}`);
  assert.ok(out.v > 2 && out.v < 8, `midpoint v=${out.v}`);
  sampleWindInto(field, 30, -70, out);
  assert.equal(out.miss, true);
  assert.ok(!Number.isFinite(out.u));
  assert.match(field.legend.source, /not a global map/);
  assert.match(field.legend.meta, /corridor/i);
});

test("a historical profile stays local instead of covering the globe", () => {
  const field = historicalWindField({
    date: "2024-06-01",
    hour_utc: 0,
    regions: [{
      stations: [{ lat: 28.5, lon: -80.6, east_mps: 6, north_mps: -2 }],
    }],
  });
  const out = { u: 0, v: 0, speed: 0, miss: false };
  sampleWindInto(field, 28.5, -80.6, out);
  assert.equal(out.miss, false);
  assert.ok(Math.abs(out.u - 6) < 1e-9);
  sampleWindInto(field, 28.5, -70, out);
  assert.equal(out.miss, true);
  assert.equal(field.global, false);
  assert.match(field.legend.meta, /sounding/i);
  const bounds = field.regions[0].bounds;
  assert.equal(boundsContain(bounds, 28.5, -80.6), true);
  assert.equal(boundsContain(bounds, 50, 10), false);
});

test("historical coverage crosses the antimeridian the short way", () => {
  const field = historicalWindField({
    regions: [{
      stations: [
        { lat: 10, lon: 179.5, east_mps: 3, north_mps: 0 },
        { lat: 10.2, lon: -179.5, east_mps: 3, north_mps: 0 },
      ],
    }],
  });
  const out = { u: 0, v: 0, speed: 0, miss: false };
  sampleWindInto(field, 10.1, 180, out);
  assert.equal(out.miss, false, "the short corridor across 180 should be inside");
  sampleWindInto(field, 10.1, 0, out);
  assert.equal(out.miss, true, "the opposite side of the earth is not this corridor");
});

test("off wind samples as a miss", () => {
  const out = { u: 1, v: 1, speed: 1, miss: false };
  sampleWindInto({ kind: "off" }, 10, 10, out);
  assert.equal(out.miss, true);
});

test("batch URL asks Open-Meteo GFS for 10 m wind", () => {
  const url = gfsSurfaceBatchUrl([{ lat: 28, lon: -90.25 }]);
  assert.match(url, /^https:\/\/api\.open-meteo\.com\/v1\/gfs\?/);
  assert.match(url, /current=wind_speed_10m,wind_direction_10m/);
  assert.match(url, /wind_speed_unit=ms/);
  assert.match(url, /latitude=28\.0000/);
  assert.match(url, /longitude=-90\.2500/);
});
