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

/**
 * One field shows the current name. Editing it renames that item.
 * Typing or picking another item's name selects that item instead.
 */
export function applyShownName(items, currentId, shown) {
  const name = typeof shown === "string" ? shown.trim() : "";
  if (!name) return { action: "ignore", id: currentId ?? null, name: "" };
  const list = items || [];
  const current = list.find((item) => String(item.id) === String(currentId)) || null;
  if (current && current.name === name) return { action: "keep", id: current.id, name };
  const match = list.find((item) => String(item.id) !== String(current?.id ?? currentId)
    && String(item.name || "").trim().toLowerCase() === name.toLowerCase());
  if (match) return { action: "select", id: match.id, name: match.name };
  return { action: "rename", id: current?.id ?? currentId ?? null, name };
}

function nameMenu(items, current, pickAttr) {
  return items
    .map((item) => {
      const currentAttr = String(item.id) === String(current.id) ? ` aria-current="true"` : "";
      return `<li><button type="button" class="name-menu-item" data-${pickAttr}="${escapeHtml(item.id)}"${currentAttr}>${escapeHtml(item.name)}</button></li>`;
    })
    .join("");
}

export function trajectoryDrawerDestHtml(input = {}) {
  const dest = resolveTrajectoryDest(input.objects, input);
  return `<div class="drawer-dest" data-role="traj-dest">
    <div class="dest-name-row">
      <button type="button" data-act="add-object" class="primary compact">+ Object</button>
      <div class="name-combo">
        <input type="text" data-role="obj-name" aria-label="Object name" aria-controls="traj-object-menu" value="${escapeHtml(dest.object.name)}" />
        <button type="button" class="name-combo-open" data-act="toggle-object-list" aria-label="Choose object" aria-controls="traj-object-menu">▾</button>
        <ul class="name-menu" id="traj-object-menu" data-role="object-menu" hidden>${nameMenu(dest.objects, dest.object, "pick-object")}</ul>
      </div>
    </div>
    <div class="dest-name-row">
      <button type="button" data-act="add-mode" class="primary compact">+ Mode</button>
      <div class="name-combo">
        <input type="text" data-role="load-mode-name" aria-label="Mode name" aria-controls="traj-mode-menu" value="${escapeHtml(dest.mode.name)}" />
        <button type="button" class="name-combo-open" data-act="toggle-mode-list" aria-label="Choose mode" aria-controls="traj-mode-menu">▾</button>
        <ul class="name-menu" id="traj-mode-menu" data-role="mode-menu" hidden>${nameMenu(dest.object.modes, dest.mode, "pick-mode")}</ul>
      </div>
    </div>
    <p class="muted wind-hint">The name in each field is where the next trajectory loads. Edit it to rename. Open the list to switch to any object, or any mode on this object.</p>
  </div>`;
}
