/** Confirmation flow for detected trajectory file formats. Pure helpers plus markup. */

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
  const formats = groups.length;
  const files = groups.reduce((count, group) => count + (group.files?.length || 0), 0);
  const noun = formats === 1 ? "trajectory file format" : "trajectory file formats";
  const lead = `${formats} unique ${noun} detected across ${files} ${files === 1 ? "file" : "files"}.`;
  const confirmed = groups.filter((group) => group.status === "accepted" || group.status === "edited").length;
  if (!confirmed) return lead;
  return `${lead} ${confirmed} already confirmed.`;
}

export function canAccept(group) {
  return group.status === "pending" && !group.needs_manual && !group.correction_required;
}

/** Decline review: load usable formats now. Hold rejected and low-confidence groups for the column marks. */
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

/** First lines of the file, unchanged, including the header and the original column text. */
export function rawFilePreview(group, limit = 8) {
  const excerpt = group.files?.[0]?.excerpt || "";
  const lines = excerpt.split("\n");
  if (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines.slice(0, limit).join("\n");
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

function channelOf(role) {
  if (role.startsWith("pos_")) return "position";
  if (role.startsWith("vel_")) return "velocity";
  if (role.startsWith("acc_")) return "acceleration";
  if (role.startsWith("orientation_")) return "orientation";
  if (role === "mass") return "mass";
  return "";
}

function selectOptions(values, selected) {
  return values
    .map((value) => `<option value="${escapeHtml(value)}"${value === selected ? " selected" : ""}>${escapeHtml(value === "" ? "—" : value)}</option>`)
    .join("");
}

function roleOptions(selected) {
  return ROLES.map((role) => `<option value="${role}"${role === selected ? " selected" : ""}>${role}</option>`).join("");
}

function unitFrameHtml(index, role, unit, frame) {
  const channel = channelOf(role);
  if (!channel) return "";
  const units = UNIT_CHOICES[channel];
  const unitValue = units.includes(unit) ? unit : units[0];
  const unitSelect = `<label>unit <select data-col="${index}" data-field="unit">${selectOptions(units, unitValue)}</select></label>`;
  if (channel !== "position" && channel !== "velocity" && channel !== "acceleration") return unitSelect;
  const frames = channel === "position" ? FRAMES : ["", ...FRAMES];
  const frameValue = frames.includes(frame) ? frame : frames[0];
  return `${unitSelect}<label>frame <select data-col="${index}" data-field="frame">${selectOptions(frames, frameValue)}</select></label>`;
}

function columnMarkHtml(index, cellText, role, classif) {
  const channel = channelOf(role);
  const frames = classif.frames || {};
  const units = classif.units || {};
  const frame = channel === "position" || channel === "velocity" || channel === "acceleration" ? frames[channel] || "" : "";
  const unit = channel ? units[channel] || "" : "";
  return `<div class="schema-cell" data-column="${index}">
    <span class="schema-cell-text">${escapeHtml(cellText)}</span>
    <label>role <select data-col="${index}" data-field="role">${roleOptions(ROLES.includes(role) ? role : "ignore")}</select></label>
    ${unitFrameHtml(index, role, unit, frame)}
  </div>`;
}

function markLineIndex(lines, headerCount, delimiter) {
  const lastHeader = headerCount > 0 ? Math.min(headerCount, lines.length) - 1 : -1;
  if (lastHeader >= 0 && splitFields(lines[lastHeader] || "", delimiter).length > 1) return lastHeader;
  for (let i = Math.max(headerCount, 0); i < lines.length; i += 1) {
    if (String(lines[i] || "").trim()) return i;
  }
  return Math.max(0, lastHeader);
}

function originFieldsHtml(group) {
  const frame = group.classification?.frames?.position || "";
  const local = frame === "NED" || frame === "NEU";
  const originFile = (group.files || []).find((file) => file.origin_lat != null);
  const origin = group.assigned_origin || {};
  const lat = origin.lat ?? originFile?.origin_lat ?? "";
  const lon = origin.lon ?? originFile?.origin_lon ?? "";
  const alt = origin.alt ?? originFile?.origin_alt_m ?? "";
  return `<div data-role="origin-fields"${local ? "" : " hidden"}>
    <p class="schema-note">Origin for a NED or NEU position column. A header origin on a file is kept.</p>
    <label>Origin latitude <input data-field="origin-lat" type="number" step="any" value="${escapeHtml(lat)}" /></label>
    <label>Origin longitude <input data-field="origin-lon" type="number" step="any" value="${escapeHtml(lon)}" /></label>
    <label>Origin altitude m <input data-field="origin-alt" type="number" step="any" value="${escapeHtml(alt)}" /></label>
  </div>`;
}

function annotatedFileHtml(group) {
  const preview = previewModel(group);
  const classif = group.classification || {};
  const lines = preview.lines.slice(0, 8);
  const headerCount = Math.max(0, preview.headerCount);
  const markIndex = lines.length ? markLineIndex(lines, headerCount, classif.delimiter) : -1;
  const markLine = markIndex >= 0 ? lines[markIndex] : "";
  const cells = splitFields(markLine, classif.delimiter);
  const count = Math.max(columnCount(group), cells.length);
  const marks = [];
  for (let i = 0; i < count; i += 1) {
    const role = classif.columns?.[`col_${i}`] || "ignore";
    marks.push(columnMarkHtml(i, cells[i] || "", role, classif));
  }
  const body = lines
    .map((line, i) => {
      const header = i < headerCount;
      const marksHtml = i === markIndex
        ? `<div class="schema-data" data-role="column-marks">${marks.join("")}${originFieldsHtml(group)}</div>`
        : "";
      return `<div class="schema-line${header ? " schema-header" : ""}" data-line-kind="${header ? "header" : "data"}">${header ? `<span class="schema-tag">header</span>` : ""}<pre class="schema-rawline">${escapeHtml(line)}</pre>${marksHtml}</div>`;
    })
    .join("");
  const delimiter = delimiterChoice(classif.delimiter);
  return `<div data-role="file-text">
    <input type="hidden" data-field="header" value="${Number(classif.header_lines) || 0}" />
    <input type="hidden" data-field="delimiter" value="${escapeHtml(delimiter)}" />
    ${body}
  </div>`;
}

function stageHtml(group, index, total) {
  const preview = previewModel(group);
  const names = (group.files || []).map((file) => file.name).join(", ");
  return `
    <div class="schema-stage">
      <div class="schema-kicker">Trajectory file format ${index + 1} of ${total} · ${escapeHtml(preview.fileName)}</div>
      <p class="schema-share">This trajectory file format is shared by ${preview.fileCount} ${preview.fileCount === 1 ? "file" : "files"}: ${escapeHtml(names)}</p>
      ${annotatedFileHtml(group)}
    </div>`;
}

function actionCard(group) {
  const note = group.classification?.reasoning ? `<p class="schema-note">${escapeHtml(group.classification.reasoning)}</p>` : "";
  const modelError = group.manual_reason
    ? `<p class="schema-error" data-role="model-error">${escapeHtml(group.manual_reason)}</p>`
    : "";
  const ask = group.needs_manual || group.editor_required || group.correction_required
    ? "Change the column marks on the line, then confirm this trajectory file format."
    : "Confirm this trajectory file format, or change a column’s role, unit, or coordinate system on the line.";
  return `<div class="schema-card">
    ${modelError}${note}
    <p class="schema-ask">${escapeHtml(ask)}</p>
    <p class="schema-error" data-role="editor-error"></p>
    <div class="schema-actions">
      <button type="button" class="primary compact" data-act="schema-save">Confirm trajectory file format</button>
      <button type="button" class="ghost compact" data-act="schema-cancel">Back</button>
    </div>
  </div>`;
}

export function reviewCardHtml(group, index, total) {
  return `${stageHtml(group, index, total)}${actionCard(group)}`;
}

export function editorHtml(group, index, total) {
  return reviewCardHtml(group, index, total);
}

export function summaryHtml(groups, errors) {
  const errorBlock = (errors || [])
    .map((error) => `<p class="schema-error">${escapeHtml(error)}</p>`)
    .join("");
  return `
    <div class="schema-summary" data-role="schema-summary">
      <p class="schema-ask">${escapeHtml(uniqueSchemaSummary(groups))}</p>
      <p class="muted wind-hint">Load without reviewing uses each detected trajectory file format. Unsupported, low-confidence, and local frames without an origin stay on the column marks. Review walks each unique trajectory file format once.</p>
      ${errorBlock}
      <div class="schema-actions">
        <button type="button" class="primary compact" data-act="schema-load">Load without reviewing</button>
        <button type="button" class="ghost compact" data-act="schema-review">Review trajectory file formats</button>
        <button type="button" class="ghost compact" data-act="schema-dismiss">Dismiss</button>
      </div>
    </div>`;
}

function columnRoles(root) {
  const indexes = [...root.querySelectorAll("[data-field='role']")]
    .map((select) => Number(select.dataset.col))
    .filter((index) => Number.isFinite(index));
  const unique = [...new Set(indexes)].sort((a, b) => a - b);
  return unique.map((index) => ({
    index,
    role: root.querySelector(`[data-col='${index}'][data-field='role']`)?.value || "ignore",
    unit: root.querySelector(`[data-col='${index}'][data-field='unit']`)?.value || "",
    frame: root.querySelector(`[data-col='${index}'][data-field='frame']`)?.value || "",
  }));
}

function channelValue(columns, channel, field, fallback) {
  const found = columns.find((column) => channelOf(column.role) === channel && column[field]);
  return found ? found[field] : fallback;
}

export function readEditor(root) {
  const value = (field) => root.querySelector(`[data-field='${field}']`)?.value ?? "";
  const columns = columnRoles(root);
  const roles = columns.map((column) => column.role);
  const lat = value("origin-lat").trim();
  const lon = value("origin-lon").trim();
  const alt = value("origin-alt").trim();
  return {
    headerLines: Number(value("header")),
    delimiter: value("delimiter"),
    positionFrame: channelValue(columns, "position", "frame", ""),
    velocityFrame: channelValue(columns, "velocity", "frame", ""),
    accelerationFrame: channelValue(columns, "acceleration", "frame", ""),
    positionUnit: channelValue(columns, "position", "unit", "m"),
    velocityUnit: channelValue(columns, "velocity", "unit", "m/s"),
    accelerationUnit: channelValue(columns, "acceleration", "unit", "m/s^2"),
    orientationUnit: channelValue(columns, "orientation", "unit", "rad"),
    massUnit: channelValue(columns, "mass", "unit", "kg"),
    roles,
    originLat: lat === "" ? null : Number(lat),
    originLon: lon === "" ? null : Number(lon),
    originAlt: alt === "" ? 0 : Number(alt),
  };
}

function frameChosen(frame) {
  return FRAMES.includes(frame);
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
  if (!frameChosen(draft.positionFrame)) problems.push("Choose a position coordinate system.");
  if (!DELIMITERS.some((item) => item.value === draft.delimiter)) problems.push("Choose a supported delimiter.");
  if (!Number.isFinite(draft.headerLines) || draft.headerLines < 0) problems.push("Header lines must be zero or more.");
  if (!UNIT_CHOICES.position.includes(draft.positionUnit)) problems.push("Choose a supported position unit.");
  if (!hasPosition(draft.positionFrame, draft.roles)) problems.push("Assign the position columns for this coordinate system.");
  if (draft.roles.some((role) => role.startsWith("vel_"))) {
    if (!frameChosen(draft.velocityFrame)) problems.push("Choose a velocity coordinate system.");
    if (!UNIT_CHOICES.velocity.includes(draft.velocityUnit)) problems.push("Choose a velocity unit.");
  }
  if (draft.roles.some((role) => role.startsWith("acc_"))) {
    if (!frameChosen(draft.accelerationFrame)) problems.push("Choose an acceleration coordinate system.");
    if (!UNIT_CHOICES.acceleration.includes(draft.accelerationUnit)) problems.push("Choose an acceleration unit.");
  }
  if (draft.roles.some((role) => role.startsWith("orientation_")) && !UNIT_CHOICES.orientation.includes(draft.orientationUnit)) {
    problems.push("Choose an orientation unit.");
  }
  if (draft.roles.includes("mass") && !UNIT_CHOICES.mass.includes(draft.massUnit)) problems.push("Choose a mass unit.");
  const local = draft.positionFrame === "NED" || draft.positionFrame === "NEU";
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
  const frames = { position: draft.positionFrame };
  if (draft.roles.some((role) => role.startsWith("vel_")) && frameChosen(draft.velocityFrame)) {
    frames.velocity = draft.velocityFrame;
  }
  if (draft.roles.some((role) => role.startsWith("acc_")) && frameChosen(draft.accelerationFrame)) {
    frames.acceleration = draft.accelerationFrame;
  }
  return {
    header_lines: draft.headerLines,
    delimiter: draft.delimiter,
    frames,
    units,
    columns,
    confidence_score: 1,
    unsupported_flag: false,
    reasoning: "Assigned on the trajectory file format.",
  };
}

export function applyEditorDraft(group, draft) {
  const local = draft.positionFrame === "NED" || draft.positionFrame === "NEU";
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

function replaceChannelControls(cell, index, role, unit, frame) {
  cell.querySelectorAll("[data-field='unit'], [data-field='frame']").forEach((node) => node.closest("label")?.remove());
  const extra = document.createElement("span");
  extra.innerHTML = unitFrameHtml(index, role, unit, frame);
  while (extra.firstChild) cell.appendChild(extra.firstChild);
}

function syncChannel(root, index) {
  const role = root.querySelector(`[data-col='${index}'][data-field='role']`)?.value || "";
  const channel = channelOf(role);
  if (!channel) return;
  const unit = root.querySelector(`[data-col='${index}'][data-field='unit']`)?.value;
  const frame = root.querySelector(`[data-col='${index}'][data-field='frame']`)?.value;
  root.querySelectorAll("[data-field='role']").forEach((select) => {
    if (select.dataset.col === String(index) || channelOf(select.value) !== channel) return;
    const otherUnit = root.querySelector(`[data-col='${select.dataset.col}'][data-field='unit']`);
    const otherFrame = root.querySelector(`[data-col='${select.dataset.col}'][data-field='frame']`);
    if (otherUnit && unit) otherUnit.value = unit;
    if (otherFrame && frame != null) otherFrame.value = frame;
  });
}

function toggleOrigin(root) {
  const block = root.querySelector("[data-role='origin-fields']");
  if (!block) return;
  const position = [...root.querySelectorAll("[data-field='role']")].find((select) => channelOf(select.value) === "position");
  const frame = position
    ? root.querySelector(`[data-col='${position.dataset.col}'][data-field='frame']`)?.value
    : "";
  block.hidden = frame !== "NED" && frame !== "NEU";
}

export function bindReview(root, handlers) {
  root.querySelector("[data-act='schema-cancel']")?.addEventListener("click", () => handlers.onCancel?.());
  root.querySelector("[data-act='schema-save']")?.addEventListener("click", () => {
    handlers.onSave?.(readEditor(root));
  });
  root.querySelector("[data-act='schema-load']")?.addEventListener("click", () => handlers.onLoad?.());
  root.querySelector("[data-act='schema-review']")?.addEventListener("click", () => handlers.onReview?.());
  root.querySelector("[data-act='schema-dismiss']")?.addEventListener("click", () => handlers.onDismiss?.());
  root.addEventListener("change", (event) => {
    const select = event.target.closest?.("select");
    if (!select || !root.contains(select) || select.dataset.col == null) return;
    const index = select.dataset.col;
    if (select.dataset.field === "role") {
      const cell = select.closest("[data-column]");
      if (cell) replaceChannelControls(cell, index, select.value, "", "");
    }
    syncChannel(root, index);
    toggleOrigin(root);
  });
}
