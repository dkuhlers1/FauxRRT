import assert from "node:assert/strict";
import fs from "node:fs";
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
  reviewHtmlAfterRead,
  formatLoadProgress,
  loadFailureMessage,
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
      frames: { position: "ECEF" },
      units: { position: "m", velocity: "m/s", acceleration: "m/s^2", orientation: "rad" },
      columns: { col_0: "time", col_1: "pos_x", col_2: "pos_y", col_3: "pos_z" },
      confidence_score: 0.9,
      unsupported_flag: false,
      reasoning: "ECEF metres.",
    },
    ...overrides,
  };
}

test("summary counts unique trajectory file formats and the files that share them", () => {
  const groups = [
    group("a", { files: [file("a.csv"), file("b.csv")] }),
    group("c"),
  ];
  assert.equal(uniqueSchemaSummary(groups), "2 unique trajectory file formats detected across 3 files.");
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
    positionFrame: "LLA",
    velocityFrame: "",
    accelerationFrame: "",
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
  assert.equal(assignmentOf(again.loadNow[0]).classification.frames.position, "LLA");
  assert.equal(assignmentOf(again.loadNow[0]).classification.frames.velocity, undefined);
});

test("accepted schemas are not asked again", () => {
  const groups = [acceptGroup(group("a")), group("b"), applyEditorDraft(group("c"), {
    headerLines: 0,
    delimiter: ",",
    positionFrame: "ECEF",
    velocityFrame: "",
    accelerationFrame: "",
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

test("review markup marks header lines and column roles on the file text", () => {
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
  assert.match(html, /data-role="file-text"/);
  assert.match(html, /<pre class="schema-rawline"># flight<\/pre>/);
  assert.match(html, /<pre class="schema-rawline">0,1,2,3<\/pre>/);
  assert.match(html, /<pre class="schema-rawline">1,4,5,6<\/pre>/);
  assert.match(html, /data-line-kind="header"/);
  assert.match(html, /data-role="column-marks"/);
  assert.match(html, /data-field="role"/);
  assert.match(html, /data-field="frame"/);
  assert.match(html, /pos_x/);
  assert.match(html, /Confirm trajectory file format/);
  assert.doesNotMatch(html, /schema-role/);
  assert.doesNotMatch(html, /Correct schema/);
  assert.doesNotMatch(html, /Confirm this schema/);
  const tabbed = editorHtml(group("tab", {
    classification: { ...group("tab").classification, delimiter: "\t" },
  }), 0, 1);
  assert.match(tabbed, /data-field="delimiter" value="tab"/);
  const manual = reviewCardHtml(group("low", { needs_manual: true, manual_reason: "confidence 0.20 is low" }), 0, 1);
  assert.match(manual, /data-role="model-error"/);
  assert.match(manual, /confidence 0.20 is low/);
  assert.match(manual, /data-act="schema-save"/);
  assert.doesNotMatch(manual, /data-field="position-frame"/);
});

test("editor requires an origin for a local frame that has none", () => {
  const pending = group("ned", {
    needs_manual: true,
    classification: {
      header_lines: 0,
      delimiter: ",",
      frames: { position: "NED" },
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
  assert.match(html, /<pre class="schema-rawline">time,x,y,z<\/pre>/);
  assert.match(html, /<pre class="schema-rawline">0,1,2,3<\/pre>/);
  assert.match(html, /Origin latitude/);
  assert.doesNotMatch(html, /Correct this schema/);
  const draft = {
    headerLines: 0,
    delimiter: ",",
    positionFrame: "NED",
    velocityFrame: "",
    accelerationFrame: "",
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

test("a failed model format opens the column marks with the error", () => {
  const html = editorHtml(
    group("bad", {
      needs_manual: true,
      editor_required: true,
      manual_reason: "eglin_keywest_6dof.csv: model output was not a JSON object",
    }),
    0,
    1,
  );
  assert.match(html, /data-role="model-error"/);
  assert.match(html, /model output was not a JSON object/);
  assert.match(html, /data-act="schema-save"/);
  assert.match(html, /Confirm trajectory file format/);
  assert.doesNotMatch(html, /Save schema/);
});

test("a rejected load command stays visible instead of becoming a blank object", () => {
  assert.equal(
    loadFailureMessage("classify_picked not allowed. Command not found"),
    "classify_picked not allowed. Command not found",
  );
  assert.equal(loadFailureMessage({ message: "commit_schema_assignments not allowed" }), "commit_schema_assignments not allowed");
  assert.equal(loadFailureMessage({}), "The trajectory load failed.");
});

test("review markup is the first screen and LLA units are per column", () => {
  const lla = group("eglin", {
    files: [{
      path: "/data/eglin_keywest_6dof.csv",
      name: "eglin_keywest_6dof.csv",
      excerpt: "time,lat,lon,alt\n0.0000,30.483200,-86.525400,26.00\n",
      origin_lat: null,
      origin_lon: null,
      origin_alt_m: null,
    }],
    classification: {
      header_lines: 1,
      delimiter: ",",
      frames: { position: "LLA" },
      units: { position: "m" },
      columns: { col_0: "time", col_1: "pos_lat", col_2: "pos_lon", col_3: "pos_alt" },
      column_units: { col_1: "deg", col_2: "deg", col_3: "m" },
      confidence_score: 0.95,
      unsupported_flag: false,
      reasoning: "Read from the header.",
    },
  });
  const html = reviewHtmlAfterRead([lla, group("other")]);
  assert.match(html, /Trajectory file format 1 of 2/);
  assert.match(html, /Confirm trajectory file format/);
  assert.match(html, /data-role="column-marks"/);
  assert.match(html, /data-field="role"/);
  assert.match(html, /data-field="unit"/);
  assert.match(html, /data-field="frame"/);
  assert.match(html, /data-col="1" data-field="unit"><option value="deg" selected>deg<\/option><option value="rad">rad<\/option>/);
  assert.match(html, /data-col="2" data-field="unit"><option value="deg" selected>deg<\/option>/);
  assert.match(html, /data-col="3" data-field="unit"><option value="m" selected>m<\/option>/);
  assert.doesNotMatch(html, /data-col="1" data-field="unit"><option value="m"/);
  assert.doesNotMatch(html, /data-col="2" data-field="unit"><option value="m"/);
  assert.doesNotMatch(html, /Load without reviewing/);
  assert.doesNotMatch(html, /data-act="schema-load"/);

  const src = fs.readFileSync(new URL("./main.js", import.meta.url), "utf8");
  const run = src.slice(src.indexOf("async function runClassify"), src.indexOf("function abandonSchemaPick"));
  assert.ok(run.indexOf("await invoke(cmd") >= 0);
  assert.ok(run.indexOf("await invoke(cmd") < run.indexOf("setBusy(true)"));
  assert.match(run, /beginSchemaReview\(\)/);
  assert.doesNotMatch(run, /openModelEditor/);
  assert.doesNotMatch(run, /llama/i);
  const fill = src.slice(src.indexOf("function fillFileLoad"), src.indexOf("function fillGenerateEntry"));
  assert.doesNotMatch(fill, /Load without reviewing/);
  assert.doesNotMatch(fill, /Llama/);
  assert.doesNotMatch(fill, /summaryHtml/);
  assert.match(src, /onCancel: \(\) => abandonSchemaPick\(\)/);
  assert.match(src, /function abandonSchemaPick\(\) {\s*schemaSession = null;/);
});

test("load progress shows bytes, files, and the device", () => {
  assert.equal(
    formatLoadProgress({
      file: "Downloading Llama 3.1 8B Instruct",
      bytes: 1_200_000_000,
      bytes_total: 4_900_000_000,
      device: "CUDA",
      done: 0,
      total: 2,
    }),
    "Downloading Llama 3.1 8B Instruct 1.2 GB / 4.9 GB · CUDA",
  );
  assert.equal(
    formatLoadProgress({ file: "Classifying track.csv", done: 1, total: 3, device: "Vulkan", bytes: 0, bytes_total: 0 }),
    "Classifying track.csv (1/3) · Vulkan",
  );
  assert.equal(
    formatLoadProgress({ file: "Classifying track.csv", done: 1, total: 2, device: "CUDA, 32 layers", bytes: 0, bytes_total: 0 }),
    "Classifying track.csv (1/2) · CUDA, 32 layers",
  );
  assert.equal(
    formatLoadProgress({ file: "Running on CUDA, 32 layers", device: "CUDA, 32 layers", done: 0, total: 1 }),
    "Running on CUDA, 32 layers",
  );
  assert.equal(
    formatLoadProgress({
      file: "Download interrupted; resuming",
      bytes: 1_200_000_000,
      bytes_total: 4_900_000_000,
      device: "CPU",
      done: 0,
      total: 1,
    }),
    "Download interrupted; resuming 1.2 GB / 4.9 GB · CPU",
  );
  assert.equal(
    formatLoadProgress({ file: "No GPU found; classifying on CPU", device: "CPU", done: 0, total: 1 }),
    "No GPU found; classifying on CPU",
  );
});
