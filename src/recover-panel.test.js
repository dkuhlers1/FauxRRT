import assert from "node:assert/strict";
import test from "node:test";

import { recoveryPanelHtml } from "./recover-panel.js";

const result = {
  phases: [
    { t_start_s: 0, t_end_s: 3, boosting: true, atmosphere: "endoatmospheric" },
    { t_start_s: 3, t_end_s: 16, boosting: false, atmosphere: "endoatmospheric" },
  ],
  thrust: {
    burn_start_s: 0.05,
    burn_end_s: 2.9,
    peak_n: 3200,
    summary_time_s: [0.05, 1.5, 2.9],
    summary_thrust_n: [3200, 3200, 1800],
    observable: true,
  },
  cd_a_m2: { boost: 0.006075, coast: 0.009544, boost_observable: true, coast_observable: true },
  cl_alpha_a_m2_per_rad: 0.038483,
  lift_observable: true,
  lift_note: null,
  product_note: "Drag and lift are a coefficient times area because the states cannot separate reference area.",
};

test("the trajectory pop-out offers recovery for the selected trajectory", () => {
  const html = recoveryPanelHtml({ track: { id: 7, name: "Calisto" }, result, error: "", busy: false });
  assert.match(html, /data-act="recover-parameters"/);
  assert.match(html, /data-role="selected-trajectory"[^>]*>Selected trajectory: Calisto</);
  assert.match(html, /data-role="product-note"[^>]*>Drag and lift are a coefficient times area because the states cannot separate reference area\.</);
  assert.match(html, /boosting · endoatmospheric/);
  assert.match(html, /not boosting · endoatmospheric/);
  assert.match(html, /Peak 3200 N/);
  assert.match(html, /data-role="cd-a-boost"[^>]*>0\.0061 m²</);
  assert.match(html, /data-role="cd-a-coast"[^>]*>0\.0095 m²</);
  assert.match(html, /data-role="cl-alpha-a"[^>]*>0\.0385 m²\/rad</);
  assert.doesNotMatch(html, /data-role="lift-note"/);
  assert.doesNotMatch(html, /disabled/);
});

test("a near-zero alpha flight marks lift not observable and does not invent a Cl", () => {
  const html = recoveryPanelHtml({
    track: { id: 3, name: "Vertical" },
    result: {
      ...result,
      cl_alpha_a_m2_per_rad: null,
      lift_observable: false,
      lift_note: "Lift is not observable: the angle of attack stays near zero, so a lift-curve slope was not fit.",
      cd_a_m2: { boost: null, coast: 0.009544, boost_observable: false, coast_observable: true },
      thrust: { observable: false },
    },
  });
  assert.match(html, /data-role="lift-note"[^>]*>Lift is not observable/);
  assert.match(html, /data-role="cl-alpha-a"[^>]*>not observable</);
  assert.match(html, /data-role="cd-a-boost"[^>]*>not observable</);
  assert.match(html, /data-role="cd-a-coast"[^>]*>0\.0095 m²</);
  assert.match(html, /Thrust: not observable/);
  assert.doesNotMatch(html, /Cl =/);
});

test("a missing channel is shown as the recovery error, and an empty selection cannot run", () => {
  const failed = recoveryPanelHtml({
    track: { id: 9, name: "Generated" },
    error: "Cannot recover missile parameters: acceleration is missing. Time, position, velocity, acceleration, orientation, and mass are required. Nothing was guessed.",
  });
  assert.match(failed, /data-role="recover-error"[^>]*>Cannot recover missile parameters: acceleration is missing/);
  assert.match(failed, /Nothing was guessed/);
  assert.match(failed, /data-role="product-note"/);

  const empty = recoveryPanelHtml({});
  assert.match(empty, /Selected trajectory: none selected/);
  assert.match(empty, /data-act="recover-parameters"[^>]*disabled/);
  assert.match(empty, /coefficient times area/);
});
