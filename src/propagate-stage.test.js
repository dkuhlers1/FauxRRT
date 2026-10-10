import assert from "node:assert/strict";
import test from "node:test";

import { propagateStage, propagationModeName } from "./propagate-stage.js";

test("a prefilled Nominal mode counts before the field is edited", () => {
  assert.equal(propagationModeName("Nominal", ""), "Nominal");
  assert.equal(propagationModeName("  Nominal  ", undefined), "Nominal");
});

test("propagate stage from an in-flight state yields a track when Nominal is only shown", async () => {
  const parent = { id: 4, time_s: 36.5, alt_m: 14000 };
  const outcome = await propagateStage({
    shownMode: "Nominal",
    storedMode: "",
    objectId: 2,
    trackId: parent.id,
    timeS: null,
    sample: { track_id: parent.id, time_s: parent.time_s, alt_m: parent.alt_m },
    stage: { ballistic_coeff: 150, dist: "point", count: 1, seed: 1 },
    invoke: async (cmd, args) => {
      assert.equal(cmd, "simulate_spent_stage");
      const spec = args.spec;
      assert.equal(spec.mode_name, "Nominal");
      assert.equal(spec.source_track_id, parent.id);
      assert.equal(spec.time_s, parent.time_s);
      assert.equal(spec.wind, undefined);
      if (!spec.mode_name || parent.alt_m < 20) return { tracks: [] };
      return {
        tracks: [{
          id: 11,
          name: `Stage t${spec.time_s.toFixed(2)}`,
          show_path: true,
          display_count: 8,
          object_id: spec.object_id,
        }],
      };
    },
  });
  assert.equal(outcome.ok, true);
  assert.equal(outcome.modeName, "Nominal");
  assert.ok(outcome.tracks.length >= 1);
  assert.match(outcome.tracks[0].name, /^Stage /);
});

test("propagate stage does not invent a track when no mode is shown or stored", async () => {
  let called = false;
  const outcome = await propagateStage({
    shownMode: "",
    storedMode: "",
    objectId: 2,
    trackId: 4,
    sample: { track_id: 4, time_s: 36.5, alt_m: 14000 },
    stage: { ballistic_coeff: 150, dist: "point" },
    invoke: async () => {
      called = true;
      return { tracks: [{ id: 1, name: "Stage" }] };
    },
  });
  assert.equal(outcome.ok, false);
  assert.equal(outcome.tracks.length, 0);
  assert.equal(called, false);
});
