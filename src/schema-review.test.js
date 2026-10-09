import assert from "node:assert/strict";
import test from "node:test";

import {
  acceptGroup,
  applyEditorDraft,
  assignmentOf,
  canAccept,
  declinePlan,
  editorHtml,
  editorProblems,
  markCorrection,
  nextPendingIndex,
  reviewCardHtml,
  uniqueSchemaSummary,
} from "./schema-review.js";

function file(name, origin) {
  return {
    path: `/data/${name}`,
    name,
    excerpt: "time,x,y,z\n0,1,2,3\n1,1,2,4\n",
    origin_lat: origin?.lat ?? null,
    origin_lon: origin?.lon ?? null,
    origin_alt_m: origin?.alt ?? null,
  };
}

function group(name, overrides = {}) {
  return {
    layout_id: name,
    needs_manual: false,
    manual_reason: "",
    status: "pending",
    loaded: false,
    files: [file(`${name}.csv`)],
    classification: {
      header_lines: 1,
      delimiter: ",",
      coordinate_system: "ECEF",
      units: { position: "m", velocity: "m/s", acceleration: "m/s^2", orientation: "rad" },
      columns: { col_0: "time", col_1: "pos_x", col_2: "pos_y", col_3: "pos_z" },
      confidence_score: 0.9,
      unsupported_flag: false,
      reasoning: "ECEF metres.",
    },
    ...overrides,
  };
}

test("summary counts unique schemas and the files that share them", () => {
  const groups = [
    group("a", { files: [file("a.csv"), file("b.csv")] }),
    group("c"),
  ];
  assert.equal(uniqueSchemaSummary(groups), "2 unique schemas detected across 3 files.");
});

test("decline loads confident schemas and holds manual ones", () => {
  const groups = [
    group("ok"),
    group("low", { needs_manual: true, manual_reason: "confidence 0.20 is low" }),
    acceptGroup(group("yes")),
  ];
  const plan = declinePlan(groups);
  assert.deepEqual(plan.loadNow.map((item) => item.layout_id), ["ok", "yes"]);
  assert.deepEqual(plan.edit.map((item) => item.layout_id), ["low"]);
  assert.equal(assignmentOf(plan.loadNow[1]).user_assigned, false);
});

test("a rejected schema is not loaded until the editor saves it", () => {
  const rejected = markCorrection(group("bad"));
  const plan = declinePlan([rejected, acceptGroup(group("kept"))]);
  assert.deepEqual(plan.loadNow.map((item) => item.layout_id), ["kept"]);
  assert.deepEqual(plan.edit.map((item) => item.layout_id), ["bad"]);
  const draft = {
    headerLines: 1,
    delimiter: ",",
    frame: "LLA",
    positionUnit: "m",
    velocityUnit: "m/s",
    accelerationUnit: "m/s^2",
    orientationUnit: "rad",
    massUnit: "kg",
    roles: ["time", "pos_lat", "pos_lon", "pos_alt"],
    originLat: null,
    originLon: null,
    originAlt: 0,
  };
  const edited = applyEditorDraft(rejected, draft);
  assert.equal(edited.status, "edited");
  assert.equal(edited.correction_required, false);
  const again = declinePlan([edited]);
  assert.equal(again.edit.length, 0);
  assert.equal(assignmentOf(again.loadNow[0]).user_assigned, true);
  assert.equal(assignmentOf(again.loadNow[0]).classification.coordinate_system, "LLA");
});

test("accepted schemas are not asked again", () => {
  const groups = [acceptGroup(group("a")), group("b"), applyEditorDraft(group("c"), {
    headerLines: 0,
    delimiter: ",",
    frame: "ECEF",
    positionUnit: "m",
    velocityUnit: "m/s",
    accelerationUnit: "m/s^2",
    orientationUnit: "rad",
    massUnit: "kg",
    roles: ["pos_x", "pos_y", "pos_z"],
    originLat: null,
    originLon: null,
    originAlt: 0,
  })];
  assert.equal(nextPendingIndex(groups), 1);
  assert.equal(canAccept(groups[0]), false);
  assert.equal(uniqueSchemaSummary(groups).includes("2 already confirmed"), true);
});

test("review markup marks header lines and column roles", () => {
  const html = reviewCardHtml(group("a", {
    files: [{
      path: "/data/a.csv",
      name: "a.csv",
      excerpt: "# flight\n0,1,2,3\n1,4,5,6\n",
      origin_lat: null,
      origin_lon: null,
      origin_alt_m: null,
    }],
  }), 0, 1);
  assert.match(html, /schema-header/);
  assert.match(html, /schema-tag">header/);
  assert.match(html, /schema-role">time/);
  assert.match(html, /schema-role">pos_x/);
  assert.match(html, /data-act="schema-yes"/);
  const tabbed = editorHtml(group("tab", {
    classification: { ...group("tab").classification, delimiter: "\t" },
  }), 0, 1);
  assert.match(tabbed, /value="tab" selected/);
  const manual = reviewCardHtml(group("low", { needs_manual: true, manual_reason: "confidence 0.20 is low" }), 0, 1);
  assert.doesNotMatch(manual, /data-act="schema-yes"/);
  assert.match(manual, /Correct schema/);
});

test("editor requires an origin for a local frame that has none", () => {
  const pending = group("ned", {
    needs_manual: true,
    classification: {
      header_lines: 0,
      delimiter: ",",
      coordinate_system: "NED",
      units: { position: "m" },
      columns: { col_0: "pos_n", col_1: "pos_e", col_2: "pos_d" },
      confidence_score: 0.4,
      unsupported_flag: false,
      reasoning: "ned",
    },
  });
  const html = editorHtml(pending, 0, 1);
  assert.match(html, /data-field="header"/);
  assert.match(html, /data-field="frame"/);
  assert.match(html, /Origin latitude/);
  const draft = {
    headerLines: 0,
    delimiter: ",",
    frame: "NED",
    positionUnit: "m",
    velocityUnit: "m/s",
    accelerationUnit: "m/s^2",
    orientationUnit: "rad",
    massUnit: "kg",
    roles: ["pos_n", "pos_e", "pos_d"],
    originLat: null,
    originLon: null,
    originAlt: 0,
  };
  assert.ok(editorProblems(draft, pending).some((problem) => problem.includes("latitude")));
  draft.originLat = 32.4;
  draft.originLon = -106.4;
  assert.equal(editorProblems(draft, pending).length, 0);
  const edited = applyEditorDraft(pending, draft);
  assert.equal(assignmentOf(edited).origin_lat, 32.4);
  assert.equal(assignmentOf(edited).paths[0], "/data/ned.csv");
});
