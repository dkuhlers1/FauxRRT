/**
 * Object and mode controls for the Trajectories pop-out.
 *
 * An empty mission shows Object 1 and a real Nominal mode. + Object and
 * + Mode add another object or mode. Editing a name updates that object
 * or mode. The selected object and mode are where the next trajectory loads.
 */

const MODE_CANDIDATES = ["Nominal", "Failure", "Abort", "Staging"];

export function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[ch]));
}

export function nextObjectName(objects) {
  const used = new Set((objects || []).map((obj) => String(obj.name || "").trim().toLowerCase()));
  let n = (objects?.length || 0) + 1;
  let name = `Object ${n}`;
  while (used.has(name.toLowerCase())) {
    n += 1;
    name = `Object ${n}`;
  }
  return name;
}

export function nextModeName(modes) {
  const used = new Set((modes || []).map((mode) => String(mode.name || "").trim().toLowerCase()));
  for (const candidate of MODE_CANDIDATES) {
    if (!used.has(candidate.toLowerCase())) return candidate;
  }
  let n = (modes?.length || 0) + 1;
  let name = `Mode ${n}`;
  while (used.has(name.toLowerCase())) {
    n += 1;
    name = `Mode ${n}`;
  }
  return name;
}

export function nominalMode(modes) {
  return (modes || []).find((mode) => String(mode.name || "").trim().toLowerCase() === "nominal") || null;
}

function withModes(object) {
  const modes = object.modes?.length
    ? object.modes
    : [{ id: "nominal", name: "Nominal", probability: 1 }];
  return { ...object, modes };
}

/** What an empty mission shows before a stored object exists. */
export function defaultTrajectoryObject() {
  return withModes({
    id: "object-1",
    name: "Object 1",
    modes: [{ id: "nominal", name: "Nominal", probability: 1 }],
    provisional: true,
  });
}

/**
 * Object and mode shown in the pop-out.
 * An empty list becomes Object 1 with Nominal selected.
 */
export function resolveTrajectoryDest(objects, selection = {}) {
  const stored = Array.isArray(objects) ? objects : [];
  const provisional = stored.length === 0;
  const list = (provisional ? [defaultTrajectoryObject()] : stored).map(withModes);
  const object = list.find((item) => String(item.id) === String(selection.objectId)) || list[0];
  const mode = object.modes.find((item) => String(item.id) === String(selection.modeId))
    || object.modes.find((item) => selection.modeName && String(item.name).toLowerCase() === String(selection.modeName).trim().toLowerCase())
    || nominalMode(object.modes)
    || object.modes[0];
  return { objects: list, object, mode, provisional };
}

export function loadTargetFromDest(dest) {
  return {
    objectId: dest.object?.id ?? null,
    objectName: dest.object?.name || "",
    modeId: dest.mode?.id ?? null,
    modeName: dest.mode?.name || "",
  };
}

export function trajectoryDrawerDestHtml(input = {}) {
  const dest = resolveTrajectoryDest(input.objects, input);
  const objectOptions = dest.objects
    .map((obj) => {
      const selected = String(obj.id) === String(dest.object.id) ? " selected" : "";
      return `<option value="${escapeHtml(obj.id)}"${selected}>${escapeHtml(obj.name)}</option>`;
    })
    .join("");
  const modeOptions = dest.object.modes
    .map((mode) => {
      const selected = String(mode.id) === String(dest.mode.id) ? " selected" : "";
      return `<option value="${escapeHtml(mode.id)}"${selected}>${escapeHtml(mode.name)}</option>`;
    })
    .join("");
  return `<div class="drawer-dest" data-role="traj-dest">
    <div class="dest-actions">
      <button type="button" data-act="add-object" class="primary compact">+ Object</button>
      <button type="button" data-act="add-mode" class="primary compact">+ Mode</button>
    </div>
    <label class="dest-row">Object
      <select data-role="traj-object" aria-label="Object that receives new trajectories">${objectOptions}</select>
      <input type="text" data-role="obj-name" aria-label="Object name" value="${escapeHtml(dest.object.name)}" />
    </label>
    <label class="dest-row">Mode
      <select data-role="traj-mode" aria-label="Mode that receives new trajectories">${modeOptions}</select>
      <input type="text" data-role="load-mode-name" aria-label="Mode name" value="${escapeHtml(dest.mode.name)}" />
    </label>
    <p class="muted wind-hint">New trajectories load into the selected object and mode. + Mode adds a mode on that object. Editing a name updates it.</p>
  </div>`;
}
