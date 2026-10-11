import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import {
  applyShownName,
  loadTargetFromDest,
  nextModeName,
  nextObjectName,
  resolveTrajectoryDest,
  trajectoryDrawerDestHtml,
} from "./trajectory-dest.js";

test("trajectories pop-out contains +Object and +Mode, and an empty mission shows Object 1", () => {
  const html = trajectoryDrawerDestHtml({ objects: [] });
  assert.match(html, /data-act="add-object"[^>]*>\+ Object</);
  assert.match(html, /data-act="add-mode"[^>]*>\+ Mode</);
  assert.match(html, /data-role="obj-name"[^>]*value="Object 1"/);
  assert.match(html, /data-role="load-mode-name"[^>]*value="Nominal"/);
  assert.match(html, /data-pick-object="object-1"[^>]*>Object 1</);
  assert.match(html, /data-pick-mode="nominal"[^>]*>Nominal</);
  assert.equal((html.match(/<input /g) || []).length, 2);
  assert.doesNotMatch(html, /<select/);
  assert.doesNotMatch(html, /<datalist/);
  assert.doesNotMatch(html, /data-role="traj-object"/);
  assert.doesNotMatch(html, /data-role="traj-mode"/);
  assert.doesNotMatch(html, /placeholder="e\.g\. Nominal"/);

  const target = loadTargetFromDest(resolveTrajectoryDest([]));
  assert.equal(target.objectName, "Object 1");
  assert.equal(target.modeName, "Nominal");
});

test("selecting another object or mode is where the next trajectory loads", () => {
  const objects = [
    {
      id: 1,
      name: "Object 1",
      modes: [
        { id: 10, name: "Nominal", probability: 1 },
        { id: 11, name: "Failure", probability: 0 },
      ],
    },
    {
      id: 2,
      name: "Booster",
      modes: [{ id: 20, name: "Nominal", probability: 1 }],
    },
  ];
  const html = trajectoryDrawerDestHtml({ objects, objectId: 2, modeId: 20 });
  assert.match(html, /data-role="obj-name"[^>]*value="Booster"/);
  const objectMenu = html.slice(html.indexOf('data-role="object-menu"'), html.indexOf('data-role="mode-menu"'));
  assert.match(objectMenu, /data-pick-object="1"[^>]*>Object 1</);
  assert.match(objectMenu, /data-pick-object="2"[^>]*>Booster</);
  assert.match(html, /data-role="load-mode-name"[^>]*value="Nominal"/);
  const boosterModes = html.slice(html.indexOf('data-role="mode-menu"'));
  assert.match(boosterModes, /data-pick-mode="20"[^>]*>Nominal</);
  assert.doesNotMatch(html, /<select/);

  const failure = trajectoryDrawerDestHtml({ objects, objectId: 1, modeId: 11 });
  assert.match(failure, /data-role="obj-name"[^>]*value="Object 1"/);
  assert.match(failure, /data-role="load-mode-name"[^>]*value="Failure"/);
  const modeMenu = failure.slice(failure.indexOf('data-role="mode-menu"'));
  assert.match(modeMenu, /data-pick-mode="10"[^>]*>Nominal</);
  assert.match(modeMenu, /data-pick-mode="11"[^>]*>Failure</);
  assert.match(failure, /data-pick-object="1"[^>]*>Object 1</);
  assert.match(failure, /data-pick-object="2"[^>]*>Booster</);
  const target = loadTargetFromDest(resolveTrajectoryDest(objects, { objectId: 1, modeId: 11 }));
  assert.equal(target.objectId, 1);
  assert.equal(target.objectName, "Object 1");
  assert.equal(target.modeName, "Failure");
});

test("editing the one name field renames, and picking another saved name switches", () => {
  const objects = [
    { id: 1, name: "Object 1" },
    { id: 2, name: "Booster" },
  ];
  assert.deepEqual(applyShownName(objects, 1, "Vehicle"), { action: "rename", id: 1, name: "Vehicle" });
  assert.deepEqual(applyShownName(objects, 1, "Booster"), { action: "select", id: 2, name: "Booster" });
  assert.deepEqual(applyShownName(objects, 1, "Object 1"), { action: "keep", id: 1, name: "Object 1" });
  const modes = [
    { id: 10, name: "Nominal" },
    { id: 11, name: "Failure" },
  ];
  assert.deepEqual(applyShownName(modes, 10, "Abort"), { action: "rename", id: 10, name: "Abort" });
  assert.deepEqual(applyShownName(modes, 10, "Failure"), { action: "select", id: 11, name: "Failure" });
  assert.equal(modes[0].name, "Nominal");
});

test("+Mode names a new mode and leaves Nominal in place", () => {
  const modes = [{ id: 1, name: "Nominal", probability: 1 }];
  assert.equal(nextModeName(modes), "Failure");
  assert.equal(modes[0].name, "Nominal");
  assert.equal(nextModeName([]), "Nominal");
  assert.equal(nextObjectName([]), "Object 1");
  assert.equal(nextObjectName([{ name: "Object 1" }]), "Object 2");
});

test("the Trajectories pane is where +Object and +Mode live, and Environment stays put", () => {
  const page = fs.readFileSync(new URL("../index.html", import.meta.url), "utf8");
  const riskStart = page.indexOf('class="panel risk-panel"');
  const risk = page.slice(riskStart, page.indexOf('id="splitter"', riskStart));
  assert.doesNotMatch(risk, /\+ Object/);
  assert.doesNotMatch(risk, /\+ Mode/);
  assert.doesNotMatch(risk, /id="btn-add-object"/);
  assert.match(page, /id="drawer-trajectories"/);
  assert.match(page, /id="btn-open-environment"/);
  assert.match(page, /id="drawer-environment"/);
  assert.match(page, /id="wind-panel"/);

  const src = fs.readFileSync(new URL("./main.js", import.meta.url), "utf8");
  const render = src.slice(src.indexOf("function renderTrajectoryDrawer"), src.indexOf("function selectLoadPath"));
  assert.match(render, /trajectoryDrawerDestHtml/);
  assert.match(render, /data-act='add-object'|data-act="add-object"/);
  assert.match(render, /data-act='add-mode'|data-act="add-mode"/);
  assert.match(src, /create_failure_mode/);
  assert.doesNotMatch(render, /A new mode name creates the mode/);
});
