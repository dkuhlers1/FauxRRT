/** Confirmation flow for detected trajectory schemas. Pure helpers plus markup. */

export const FRAMES = ["NED", "NEU", "LLA", "ECEF", "ECI"];

export const DELIMITERS = [
  { value: ",", label: "Comma" },
  { value: "tab", label: "Tab" },
  { value: ";", label: "Semicolon" },
  { value: "|", label: "Pipe" },
  { value: "whitespace", label: "Whitespace" },
];

function delimiterChoice(value) {
  if (value === "\t" || value === "\\t" || value === "tab") return "tab";
  return DELIMITERS.some((item) => item.value === value) ? value : ",";
}

export const ROLES = [
  "time",
  "pos_x",
  "pos_y",
  "pos_z",
  "pos_lat",
  "pos_lon",
  "pos_alt",
  "pos_n",
  "pos_e",
  "pos_d",
  "pos_u",
  "vel_x",
  "vel_y",
  "vel_z",
  "vel_n",
  "vel_e",
  "vel_d",
  "vel_u",
  "acc_x",
  "acc_y",
  "acc_z",
  "acc_n",
  "acc_e",
  "acc_d",
  "acc_u",
  "orientation_roll",
  "orientation_pitch",
  "orientation_yaw",
  "mass",
  "ignore",
];

const UNIT_CHOICES = {
  position: ["m", "km", "ft"],
  velocity: ["m/s", "km/s", "ft/s"],
  acceleration: ["m/s^2", "ft/s^2"],
  orientation: ["rad", "deg"],
  mass: ["kg", "lbm"],
};

function escapeHtml(value) {
  return String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function loadFailureMessage(err) {
  if (err == null || err === "") return "The trajectory load failed.";
  if (typeof err === "string") return err;
  if (typeof err.message === "string" && err.message) return err.message;
  const text = String(err);
  return text === "[object Object]" ? "The trajectory load failed." : text;
}

export function uniqueSchemaSummary(groups) {
  const schemas = groups.length;
  const files = groups.reduce((count, group) => count + (group.files?.length || 0), 0);
  const lead = `${schemas} unique ${schemas === 1 ? "schema" : "schemas"} detected across ${files} ${files === 1 ? "file" : "files"}.`;
  const confirmed = groups.filter((group) => group.status === "accepted" || group.status === "edited").length;
  if (!confirmed) return lead;
  return `${lead} ${confirmed} already confirmed.`;
}

export function canAccept(group) {
  return group.status === "pending" && !group.needs_manual && !group.correction_required;
}

/** Decline review: load usable schemas now. Hold rejected and low-confidence groups for the editor. */
export function declinePlan(groups) {
  const loadNow = [];
  const edit = [];
  for (const group of groups) {
    if (group.loaded) continue;
    if (group.correction_required && group.status !== "edited") {
      edit.push(group);
      continue;
    }
    if (group.status === "edited" || group.status === "accepted" || !group.needs_manual) {
      loadNow.push(group);
    } else {
      edit.push(group);
    }
  }
  return { loadNow, edit };
}

export function nextPendingIndex(groups) {
  return groups.findIndex((group) => group.status === "pending" && !group.loaded);
}

export function acceptGroup(group) {
  if (!canAccept(group)) return group;
  return { ...group, status: "accepted" };
}

export function markCorrection(group) {
  return { ...group, status: "pending", correction_required: true };
}

export function assignmentOf(group) {
  const origin = group.assigned_origin;
  return {
    paths: (group.files || []).map((file) => file.path),
    classification: group.classification,
    origin_lat: origin ? origin.lat : null,
    origin_lon: origin ? origin.lon : null,
    origin_alt_m: origin ? origin.alt ?? 0 : null,
    user_assigned: group.status === "edited",
  };
}

export function splitFields(line, delimiter) {
  if (!line) return [];
  if (delimiter === "whitespace" || delimiter === "space") {
    return line.trim().split(/\s+/).filter((part) => part.length > 0);
  }
  const ch = delimiter === "\\t" || delimiter === "tab" ? "\t" : delimiter || ",";
  return line.split(ch).map((part) => part.trim());
}

export function previewModel(group) {
  const excerpt = group.files?.[0]?.excerpt || "";
  const lines = excerpt.split(/\n/);
  if (lines.length && lines[lines.length - 1] === "") lines.pop();
  const headerCount = Number(group.classification?.header_lines) || 0;
  let dataIndex = -1;
  for (let i = headerCount; i < lines.length; i += 1) {
    if (lines[i].trim()) {
      dataIndex = i;
      break;
    }
  }
  const columns = group.classification?.columns || {};
  const dataLine = dataIndex >= 0 ? lines[dataIndex] : "";
  const cells = splitFields(dataLine, group.classification?.delimiter).map((text, index) => ({
    text,
    role: columns[`col_${index}`] || "ignore",
    index,
  }));
  return {
    lines,
    headerCount,
    dataIndex,
    cells,
    fileName: group.files?.[0]?.name || "file",
    fileCount: group.files?.length || 0,
  };
}

function columnCount(group) {
  const columns = group.classification?.columns || {};
  let max = 0;
  for (const key of Object.keys(columns)) {
    const index = Number(String(key).replace(/^col_/, ""));
    if (Number.isFinite(index)) max = Math.max(max, index + 1);
  }
  return Math.max(max, previewModel(group).cells.length, 1);
}

function markedLines(group) {
  const preview = previewModel(group);
  return preview.lines
    .map((line, index) => {
      if (index < preview.headerCount) {
        return `<div class="schema-line schema-header"><span class="schema-tag">header</span><span class="schema-text">${escapeHtml(line)}</span></div>`;
      }
      if (index === preview.dataIndex) {
        const cells = preview.cells
          .map(
            (cell) =>
              `<span class="schema-cell"><span class="schema-role">${escapeHtml(cell.role)}</span><span class="schema-value">${escapeHtml(cell.text)}</span></span>`,
          )
          .join("");
        return `<div class="schema-line schema-data">${cells}</div>`;
      }
      return `<div class="schema-line"><span class="schema-text">${escapeHtml(line)}</span></div>`;
    })
    .join("");
}

function stageHtml(group, index, total) {
  const preview = previewModel(group);
  const names = (group.files || []).map((file) => file.name).join(", ");
  return `
    <div class="schema-stage">
      <div class="schema-kicker">Schema ${index + 1} of ${total} · ${escapeHtml(preview.fileName)}</div>
      <p class="schema-share">This layout is shared by ${preview.fileCount} ${preview.fileCount === 1 ? "file" : "files"}: ${escapeHtml(names)}</p>
      ${markedLines(group)}
    </div>`;
}

export function reviewCardHtml(group, index, total) {
  const frame = group.classification?.coordinate_system || "";
  const note = group.classification?.reasoning ? `<p class="schema-note">${escapeHtml(group.classification.reasoning)}</p>` : "";
  const ask = canAccept(group)
    ? `<p class="schema-ask">Confirm this ${escapeHtml(frame)} schema?</p>
       <div class="schema-actions">
         <button type="button" class="primary compact" data-act="schema-yes">Yes</button>
         <button type="button" class="ghost compact" data-act="schema-no">No</button>
         <button type="button" class="ghost compact" data-act="schema-cancel">Back</button>
       </div>`
    : `<p class="schema-ask">${escapeHtml(group.manual_reason || "This schema needs a manual assignment.")} Correct it before these files load.</p>
       <div class="schema-actions">
         <button type="button" class="primary compact" data-act="schema-no">Correct schema</button>
         <button type="button" class="ghost compact" data-act="schema-cancel">Back</button>
       </div>`;
  return `${stageHtml(group, index, total)}<div class="schema-card">${note}${ask}</div>`;
}

function selectOptions(values, selected) {
  return values
    .map((value) => `<option value="${escapeHtml(value)}"${value === selected ? " selected" : ""}>${escapeHtml(value)}</option>`)
    .join("");
}

function unitSelect(kind, selected) {
  const values = UNIT_CHOICES[kind];
  const current = values.includes(selected) ? selected : values[0];
  return `<label>${kind[0].toUpperCase()}${kind.slice(1)} unit
    <select data-field="${kind}">${selectOptions(values, current)}</select>
  </label>`;
}

export function editorHtml(group, index, total) {
  const classif = group.classification || {};
  const units = classif.units || {};
  const count = columnCount(group);
  const preview = previewModel(group);
  const roleRows = [];
  for (let i = 0; i < count; i += 1) {
    const selected = classif.columns?.[`col_${i}`] || preview.cells[i]?.role || "ignore";
    const sample = preview.cells[i]?.text || "";
    roleRows.push(`<label>Column ${i}${sample ? ` · ${escapeHtml(sample)}` : ""}
      <select data-col="${i}">${ROLES.map((role) => `<option value="${role}"${role === selected ? " selected" : ""}>${role}</option>`).join("")}</select>
    </label>`);
  }
  const frame = FRAMES.includes(classif.coordinate_system) ? classif.coordinate_system : "ECEF";
  const delimiter = delimiterChoice(classif.delimiter);
  const local = frame === "NED" || frame === "NEU";
  const originFile = (group.files || []).find((file) => file.origin_lat != null);
  const origin = group.assigned_origin || {};
  const lat = origin.lat ?? originFile?.origin_lat ?? "";
  const lon = origin.lon ?? originFile?.origin_lon ?? "";
  const alt = origin.alt ?? originFile?.origin_alt_m ?? "";
  return `${stageHtml(group, index, total)}
    <div class="schema-card schema-editor">
      <p class="schema-ask">Correct this schema. The change applies to every file that shares it.</p>
      <div class="schema-form">
        <label>Header lines <input data-field="header" type="number" min="0" step="1" value="${Number(classif.header_lines) || 0}" /></label>
        <label>Delimiter <select data-field="delimiter">${DELIMITERS.map((item) => `<option value="${escapeHtml(item.value)}"${item.value === delimiter ? " selected" : ""}>${item.label}</option>`).join("")}</select></label>
        <label>Coordinate system <select data-field="frame">${selectOptions(FRAMES, frame)}</select></label>
        ${unitSelect("position", units.position || "m")}
        ${unitSelect("velocity", units.velocity || "m/s")}
        ${unitSelect("acceleration", units.acceleration || "m/s^2")}
        ${unitSelect("orientation", units.orientation || "rad")}
        ${unitSelect("mass", units.mass || "kg")}
        ${roleRows.join("")}
      </div>
      <div data-role="origin-fields"${local ? "" : " hidden"}>
        <p class="schema-note">Origin for NED and NEU files in this schema that do not already name one. A header origin on a file is kept.</p>
        <div class="schema-form">
          <label>Origin latitude <input data-field="origin-lat" type="number" step="any" value="${escapeHtml(lat)}" /></label>
          <label>Origin longitude <input data-field="origin-lon" type="number" step="any" value="${escapeHtml(lon)}" /></label>
          <label>Origin altitude m <input data-field="origin-alt" type="number" step="any" value="${escapeHtml(alt)}" /></label>
        </div>
      </div>
      <p class="schema-error" data-role="editor-error"></p>
      <div class="schema-actions">
        <button type="button" class="primary compact" data-act="schema-save">Save schema</button>
        <button type="button" class="ghost compact" data-act="schema-cancel">Back</button>
      </div>
    </div>`;
}

export function summaryHtml(groups, errors) {
  const errorBlock = (errors || [])
    .map((error) => `<p class="schema-error">${escapeHtml(error)}</p>`)
    .join("");
  return `
    <div class="schema-summary" data-role="schema-summary">
      <p class="schema-ask">${escapeHtml(uniqueSchemaSummary(groups))}</p>
      <p class="muted wind-hint">Load without reviewing uses each detected schema. Unsupported, low-confidence, and local frames without an origin open in the editor instead. Review walks each unique schema once.</p>
      ${errorBlock}
      <div class="schema-actions">
        <button type="button" class="primary compact" data-act="schema-load">Load without reviewing</button>
        <button type="button" class="ghost compact" data-act="schema-review">Review schemas</button>
        <button type="button" class="ghost compact" data-act="schema-dismiss">Dismiss</button>
      </div>
    </div>`;
}

export function readEditor(root) {
  const value = (field) => root.querySelector(`[data-field='${field}']`)?.value ?? "";
  const roles = [...root.querySelectorAll("[data-col]")].map((select) => select.value);
  const lat = value("origin-lat").trim();
  const lon = value("origin-lon").trim();
  const alt = value("origin-alt").trim();
  return {
    headerLines: Number(value("header")),
    delimiter: value("delimiter"),
    frame: value("frame"),
    positionUnit: value("position"),
    velocityUnit: value("velocity"),
    accelerationUnit: value("acceleration"),
    orientationUnit: value("orientation"),
    massUnit: value("mass"),
    roles,
    originLat: lat === "" ? null : Number(lat),
    originLon: lon === "" ? null : Number(lon),
    originAlt: alt === "" ? 0 : Number(alt),
  };
}

function hasPosition(frame, roles) {
  const has = (...names) => names.every((name) => roles.includes(name));
  if (frame === "LLA") return has("pos_lat", "pos_lon");
  if (frame === "ECEF" || frame === "ECI") return has("pos_x", "pos_y", "pos_z");
  if (frame === "NED") return has("pos_n", "pos_e", "pos_d") || has("pos_x", "pos_y", "pos_z");
  if (frame === "NEU") return has("pos_n", "pos_e", "pos_u") || has("pos_x", "pos_y", "pos_z");
  return false;
}

export function editorProblems(draft, group) {
  const problems = [];
  if (!FRAMES.includes(draft.frame)) problems.push("Choose a supported coordinate system.");
  if (!DELIMITERS.some((item) => item.value === draft.delimiter)) problems.push("Choose a supported delimiter.");
  if (!Number.isFinite(draft.headerLines) || draft.headerLines < 0) problems.push("Header lines must be zero or more.");
  if (!UNIT_CHOICES.position.includes(draft.positionUnit)) problems.push("Choose a supported position unit.");
  if (!hasPosition(draft.frame, draft.roles)) problems.push("Assign the position columns for this coordinate system.");
  if (draft.roles.some((role) => role.startsWith("vel_")) && !UNIT_CHOICES.velocity.includes(draft.velocityUnit)) {
    problems.push("Choose a velocity unit.");
  }
  if (draft.roles.some((role) => role.startsWith("acc_")) && !UNIT_CHOICES.acceleration.includes(draft.accelerationUnit)) {
    problems.push("Choose an acceleration unit.");
  }
  if (draft.roles.some((role) => role.startsWith("orientation_")) && !UNIT_CHOICES.orientation.includes(draft.orientationUnit)) {
    problems.push("Choose an orientation unit.");
  }
  if (draft.roles.includes("mass") && !UNIT_CHOICES.mass.includes(draft.massUnit)) problems.push("Choose a mass unit.");
  const local = draft.frame === "NED" || draft.frame === "NEU";
  const missingOrigin = (group.files || []).some((file) => file.origin_lat == null || file.origin_lon == null);
  if (local && missingOrigin) {
    if (!Number.isFinite(draft.originLat) || draft.originLat < -90 || draft.originLat > 90) {
      problems.push("Enter an origin latitude.");
    }
    if (!Number.isFinite(draft.originLon) || draft.originLon < -180 || draft.originLon > 180) {
      problems.push("Enter an origin longitude.");
    }
  }
  return problems;
}

export function buildClassification(draft) {
  const columns = {};
  draft.roles.forEach((role, index) => {
    columns[`col_${index}`] = role || "ignore";
  });
  const units = {
    position: draft.positionUnit,
    velocity: draft.velocityUnit,
    acceleration: draft.accelerationUnit,
    orientation: draft.orientationUnit,
  };
  if (draft.roles.includes("mass")) units.mass = draft.massUnit;
  return {
    header_lines: draft.headerLines,
    delimiter: draft.delimiter,
    coordinate_system: draft.frame,
    units,
    columns,
    confidence_score: 1,
    unsupported_flag: false,
    reasoning: "Assigned in the schema editor.",
  };
}

export function applyEditorDraft(group, draft) {
  const local = draft.frame === "NED" || draft.frame === "NEU";
  const assigned =
    local && Number.isFinite(draft.originLat) && Number.isFinite(draft.originLon)
      ? { lat: draft.originLat, lon: draft.originLon, alt: Number.isFinite(draft.originAlt) ? draft.originAlt : 0 }
      : null;
  return {
    ...group,
    classification: buildClassification(draft),
    status: "edited",
    needs_manual: false,
    correction_required: false,
    manual_reason: "",
    assigned_origin: assigned,
  };
}

export function formatLoadProgress(payload) {
  if (!payload) return "";
  const file = payload.file || "";
  const device = payload.device && !file.includes(payload.device) ? ` · ${payload.device}` : "";
  const bytesTotal = Number(payload.bytes_total) || 0;
  if (bytesTotal > 0) {
    return `${file} ${formatByteCount(payload.bytes)} / ${formatByteCount(bytesTotal)}${device}`;
  }
  const done = Number(payload.done) || 0;
  const total = Number(payload.total) || 0;
  if (done > 0 && total > 0) return `${file} (${done}/${total})${device}`;
  return `${file}${device}`;
}

function formatByteCount(value) {
  const n = Number(value) || 0;
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)} MB`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(0)} KB`;
  return `${Math.round(n)} B`;
}

export function bindReview(root, handlers) {
  root.querySelector("[data-act='schema-yes']")?.addEventListener("click", () => handlers.onYes?.());
  root.querySelector("[data-act='schema-no']")?.addEventListener("click", () => handlers.onNo?.());
  root.querySelector("[data-act='schema-cancel']")?.addEventListener("click", () => handlers.onCancel?.());
  root.querySelector("[data-act='schema-save']")?.addEventListener("click", () => handlers.onSave?.(readEditor(root)));
  root.querySelector("[data-act='schema-load']")?.addEventListener("click", () => handlers.onLoad?.());
  root.querySelector("[data-act='schema-review']")?.addEventListener("click", () => handlers.onReview?.());
  root.querySelector("[data-act='schema-dismiss']")?.addEventListener("click", () => handlers.onDismiss?.());
  root.querySelector("[data-field='frame']")?.addEventListener("change", () => {
    const block = root.querySelector("[data-role='origin-fields']");
    if (!block) return;
    const frame = root.querySelector("[data-field='frame']")?.value;
    block.hidden = frame !== "NED" && frame !== "NEU";
  });
}
