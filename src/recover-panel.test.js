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
  assert.match(html, /C_cross cylinder extrapolation/);
  assert.match(html, /data-role="c-cross"[^>]*>not observable</);
  assert.match(html, /cylinder extrapolation was not fit/);
  assert.doesNotMatch(html, /data-role="c-cross-assumed"/);
  assert.doesNotMatch(html, /data-role="lift-note"/);
  assert.doesNotMatch(html, /data-role="derived-note"/);
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
  assert.match(html, /data-role="c-cross"[^>]*>not observable</);
  assert.match(html, /cylinder extrapolation/);
});

test("a fitted broadside area is labeled a cylinder extrapolation, and 1.2·L·D stays assumed", () => {
  const html = recoveryPanelHtml({
    track: { id: 4, name: "High alpha" },
    result: {
      ...result,
      c_cross_m2: 0.252,
      c_cross_observable: true,
      c_cross_note: "Cylinder extrapolation of broadside drag area. At 90° only the second term remains.",
      c_cross_assumed_m2: 0.168,
      c_cross_assumed_note: "Assumed from 1.2·L·D for a circular cylinder in subsonic flow. Not fit from the states.",
    },
  });
  assert.match(html, /data-role="c-cross"[^>]*>0\.2520 m²</);
  assert.match(html, /C_cross cylinder extrapolation/);
  assert.match(html, /data-role="c-cross-note"[^>]*>Cylinder extrapolation/);
  assert.match(html, /data-role="c-cross-assumed"[^>]*>assumed 0\.1680 m²</);
  assert.match(html, /data-role="c-cross-assumed-note"[^>]*>[^<]*Not fit from the states/);
  assert.doesNotMatch(html, /data-role="c-cross"[^>]*>assumed/);
});

test("derived velocity and acceleration are one short note on the trajectory pop-out", () => {
  const html = recoveryPanelHtml({
    track: { id: 8, name: "Positions" },
    result: {
      ...result,
      derived_channels: ["velocity", "acceleration"],
      derived_note: "Velocity and acceleration were derived from position.",
    },
  });
  assert.match(html, /data-role="derived-note"[^>]*>Velocity and acceleration were derived from position\./);
  assert.equal(html.match(/data-role="derived-note"/g).length, 1);

  const fromVelocity = recoveryPanelHtml({
    track: { id: 8, name: "Positions" },
    result: {
      ...result,
      derived_channels: ["acceleration"],
      derived_note: "Acceleration was derived from velocity.",
    },
  });
  assert.match(fromVelocity, /data-role="derived-note"[^>]*>Acceleration was derived from velocity\./);
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
