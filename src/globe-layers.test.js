import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import {
  boatOnGlobe,
  globeLayersToShow,
  isolineOnGlobe,
  kdeGridOnGlobe,
  layerPresenceFromCounts,
  trackDrawsOnGlobe,
  vesselCellOnGlobe,
  windFieldOnGlobe,
} from "./globe-layers.js";

const ids = (counts) => globeLayersToShow(layerPresenceFromCounts(counts)).map((row) => row.id);

test("an empty layer is omitted and a layer with data is shown", () => {
  assert.deepEqual(ids({}), []);

  assert.deepEqual(ids({ impacts: 0, boats: 0, trajectories: 0, isolines: 0 }), []);
  assert.deepEqual(ids({ impacts: 1 }), ["impacts"]);
  assert.deepEqual(ids({ boats: 1 }), ["boats"]);
  assert.deepEqual(ids({ trajectories: 2 }), ["trajectories"]);
  assert.deepEqual(ids({ wind: true }), ["wind"]);
  assert.deepEqual(ids({ kde: true }), ["kde"]);
  assert.deepEqual(ids({ isolines: 4 }), ["isolines"]);

  assert.deepEqual(ids({ impacts: 1, boats: 2 }), ["impacts", "boats"]);
  assert.deepEqual(ids({ impacts: 0, boats: 2 }), ["boats"]);
  assert.deepEqual(ids({ impacts: 1, boats: 0 }), ["impacts"]);

  assert.equal(trackDrawsOnGlobe({ show_path: false, display_lla: [1, 2, 3, 4, 5, 6] }), false);
  assert.equal(trackDrawsOnGlobe({ visible: false, display_lla: [1, 2, 3, 4, 5, 6] }), false);
  assert.equal(trackDrawsOnGlobe({ display_lla: [1, 2, 3] }), false);
  assert.equal(trackDrawsOnGlobe({ display_lla: [1, 2, 3, 4, 5, 6] }), true);
  assert.equal(windFieldOnGlobe(null), false);
  assert.equal(windFieldOnGlobe({ kind: "off" }), false);
  assert.equal(windFieldOnGlobe({ kind: "historical", loading: true, regions: [{}] }), false);
  assert.equal(windFieldOnGlobe({ kind: "historical", regions: [] }), false);
  assert.equal(windFieldOnGlobe({ kind: "historical", regions: [{ bounds: {} }] }), true);
  assert.equal(windFieldOnGlobe({ kind: "constant" }), true);
  assert.equal(kdeGridOnGlobe({ values: [0], max_value: 0 }), false);
  assert.equal(kdeGridOnGlobe({ values: [1], max_value: 0.2 }), true);
  assert.equal(isolineOnGlobe({ line: [1, 2] }), false);
  assert.equal(isolineOnGlobe({ line: [1, 2, 3, 4] }), true);
  assert.equal(vesselCellOnGlobe({ boundary: [1, 2, 3] }), false);
  assert.equal(vesselCellOnGlobe({ boundary: [1, 2, 3, 4, 5, 6] }), true);
  assert.equal(boatOnGlobe({ lon: 10 }), false);
  assert.equal(boatOnGlobe({ lon: -90, lat: 28, visible: false }), false);
  assert.equal(boatOnGlobe({ lon: -90, lat: 28 }), true);
});

test("boundary rows wait until a hull can close", () => {
  assert.deepEqual(ids({ impacts: 2, terminate: 2 }), ["impacts"]);
  assert.deepEqual(ids({ impacts: 3, terminate: 3 }), ["impacts", "iip", "terminate"]);
});

test("the layer list overlays the globe at the bottom right", () => {
  const html = fs.readFileSync(new URL("../index.html", import.meta.url), "utf8");
  const globeStart = html.indexOf('<div id="globe">');
  const globeEnd = html.indexOf('<aside id="side-drawer"');
  assert.ok(globeStart >= 0 && globeEnd > globeStart);
  const globe = html.slice(globeStart, globeEnd);
  const beforeGlobe = html.slice(0, globeStart);
  assert.match(globe, /id="globe-layers"/);
  assert.doesNotMatch(beforeGlobe, /id="globe-layers"/);
  assert.doesNotMatch(beforeGlobe, /id="show-impacts"/);
  assert.match(globe, /class="globe-layer hidden" data-layer="impacts"/);
  assert.match(globe, /class="globe-layer hidden" data-layer="trajectories"/);
  assert.match(globe, /class="globe-layer hidden" data-layer="wind"/);
  assert.match(globe, /class="globe-layer hidden" data-layer="kde"/);
  assert.match(globe, /class="globe-layer hidden" data-layer="isolines"/);
  assert.match(globe, /class="globe-layer hidden" data-layer="boats"/);

  const css = fs.readFileSync(new URL("./styles.css", import.meta.url), "utf8");
  const block = css.slice(css.indexOf(".globe-layers {"), css.indexOf("}", css.indexOf(".globe-layers {")) + 1);
  assert.match(block, /position:\s*absolute/);
  assert.match(block, /flex-direction:\s*column/);
  assert.match(block, /right:\s*8px/);
  assert.match(block, /bottom:\s*36px/);
  assert.match(block, /max-width:\s*calc\(100% - 16px\)/);
  assert.match(block, /max-height:\s*calc\(100% - 108px\)/);
});
