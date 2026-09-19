import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  addTracks,
  beginGlobePick,
  bindBoatInspect,
  boatBounds,
  cancelGlobePick,
  clearTracks,
  createGlobe,
  currentKdeGrid,
  fitAll,
  flyToBounds,
  highlight,
  highlightBoat,
  impactBounds,
  kdeBounds,
  removeTrack,
  resizeGlobe,
  setBoats,
  setBoatsVisible,
  setImagery,
  setImpactPoints,
  setImpactVisible,
  setIipHullVisible,
  setKdeGrid,
  setKdeVisible,
  setSiteChangeHandler,
  setSiteMarkers,
  setStateMarker,
  setTerminateHull,
  setTerminateHullVisible,
  setVisible,
} from "./globe.js";
import { enrichBoat, parseBoatKml, scoreBoatsAgainstGrid } from "./boats-kml.js";

const tracks = new Map();
let boats = [];
let selectedId = null;
let selectedBoatId = null;
let hoveredBoatId = null;
let lastKdeGrid = null;
let riskModel = { objects: [], unassigned: [] };
let pendingFocus = null;
let mission = { name: "Untitled mission", path: null, dirty: false, recent: [] };
const generateDrafts = new Map();
const generateEngine = new Map();
const rocketpyDrafts = new Map();
const objectMethod = new Map();
let rocketpyOpenId = null;
let rocketpyTab = "env";
let rocketpyStatus = { checked: false, available: false };
let missionWind = {
  type: "off",
  speed_mps: 15,
  from_deg: 270,
  date: "",
  hour_utc: 12,
  source: "",
};
const objectLoadName = new Map();
/** @type {number | null | undefined} undefined = expand first object */
let expandedObjectId = undefined;
let pendingExpandLast = false;
const sectionCollapsed = { wind: false, boats: false };
let catalogs = [];
const simDraft = {
  destObjectId: null,
  kind: "stage",
  trackId: 0,
  timeS: null,
  sample: null,
  catalogId: 1,
  seed: 1,
  showCatalog: false,
  stage: { ballistic_coeff: 150, dist: "point", count: 20, tMin: null, tMax: null, sigma: 2, nSigma: 3, seed: 1 },
  fts: {},
  nav: {
    maxG: 5,
    durationS: 5,
    side: "both",
    times: [],
    everyS: 10,
    sustain: true,
  },
};
let sampleTimer = null;
let pickKind = null;
let impactSeq = 0;
let kdeSeq = 0;
let kdeTimer = null;
let lastPointKey = "";
let lastWeightKey = "";
let lastImpactSummary = "";
const KDE_IDLE_MS = 1000;

const els = {
  missionName: document.getElementById("mission-name"),
  missionDirty: document.getElementById("mission-dirty"),
  missionPath: document.getElementById("mission-path"),
  missionNew: document.getElementById("btn-mission-new"),
  missionOpen: document.getElementById("btn-mission-open"),
  missionSave: document.getElementById("btn-mission-save"),
  fit: document.getElementById("btn-fit"),
  progress: document.getElementById("progress"),
  statTracks: document.getElementById("stat-tracks"),
  statPoints: document.getElementById("stat-points"),
  statTime: document.getElementById("stat-time"),
  addObject: document.getElementById("btn-add-object"),
  objectList: document.getElementById("object-list"),
  unassigned: document.getElementById("unassigned"),
  kdeStatus: document.getElementById("kde-status"),
  showImpacts: document.getElementById("show-impacts"),
  showKde: document.getElementById("show-kde"),
  showBoats: document.getElementById("show-boats"),
  showIipBoundary: document.getElementById("show-iip-boundary"),
  showTerminateBoundary: document.getElementById("show-terminate-boundary"),
  imagery: document.getElementById("imagery-kind"),
  windFields: document.getElementById("wind-fields"),
  loadBoats: document.getElementById("btn-load-boats"),
  sampleBoats: document.getElementById("btn-sample-boats"),
  clearBoats: document.getElementById("btn-clear-boats"),
  boatList: document.getElementById("boat-list"),
  boatKmlInput: document.getElementById("boat-kml-input"),
  boatDetail: document.getElementById("boat-detail"),
  boatRisk: document.getElementById("boat-risk"),
  boatTooltip: document.getElementById("boat-tooltip"),
  rocketpyOverlay: document.getElementById("rocketpy-overlay"),
};

els.fit?.addEventListener("click", () => {
  const list = [...tracks.values()].filter((t) => t.visible !== false);
  if (!list.length && !boats.length) {
    els.progress.textContent = "Nothing to fit — add a trajectory first.";
    return;
  }
  flyToImpactsOrTracks(list);
});
els.addObject?.addEventListener("click", () => createObject());
els.showImpacts?.addEventListener("change", () => {
  setImpactVisible(els.showImpacts.checked);
  persistMissionUi();
});
els.showKde?.addEventListener("change", () => {
  setKdeVisible(els.showKde.checked);
  persistMissionUi();
});
els.showBoats?.addEventListener("change", () => {
  setBoatsVisible(els.showBoats.checked);
  persistMissionUi();
});
els.showIipBoundary?.addEventListener("change", () => {
  setIipHullVisible(els.showIipBoundary.checked);
  persistMissionUi();
});
els.showTerminateBoundary?.addEventListener("change", () => {
  setTerminateHullVisible(els.showTerminateBoundary.checked);
  persistMissionUi();
});
els.loadBoats?.addEventListener("click", () => runLoadBoats());
els.sampleBoats?.addEventListener("click", () => runSampleBoats());
els.clearBoats?.addEventListener("click", () => runClearBoats());
els.boatKmlInput?.addEventListener("change", () => importBoatFiles(els.boatKmlInput.files));
els.missionNew?.addEventListener("click", () => runMission("new_mission", true));
els.missionOpen?.addEventListener("click", () => runMission("open_mission", true));
els.missionSave?.addEventListener("click", () => runMission(mission.path ? "save_mission" : "save_mission_as", false));
els.missionName?.addEventListener("change", async () => {
  try {
    mission = await invoke("rename_mission", { name: els.missionName.value });
    renderMission();
  } catch (err) {
    els.progress.textContent = String(err);
  }
});

setupSplitter();
bindWindPanel();
bindSectionToggles();
bindSimActions();
bindBoatListHover();
document.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape" && rocketpyOpenId != null) closeRocketPyBuilder();
});
try {
  await createGlobe(document.getElementById("globe"));
  setSiteChangeHandler(onGlobeSiteChange);
  bindBoatInspect({
    hover: onGlobeBoatHover,
    click: (id) => selectBoat(id),
  });
} catch (err) {
  console.error(err);
  if (els.progress) els.progress.textContent = `Globe failed: ${err}`;
}
try {
  await listen("load-progress", ({ payload }) => {
    els.progress.textContent = `Parsing ${payload.done}/${payload.total}  ${payload.file}`;
  });
} catch {
  /* previewed outside the Tauri shell */
}
try {
  const snap = await invoke("open_last_mission");
  await applySnapshot(snap, false);
} catch {
  await refreshRisk();
  await refreshMission();
  render();
}

async function runLoad(cmd, dest) {
  if (!dest?.object_id || !dest?.mode_name) {
    els.progress.textContent = "Add an object, name a mode, then Load files or Generate.";
    return;
  }
  setBusy(true);
  els.progress.textContent = "Opening…";
  try {
    const result = await invoke(cmd, ipcArgs({
      objectId: dest.object_id,
      modeName: dest.mode_name,
    }));
    for (const track of result.tracks) {
      tracks.set(track.id, track);
    }
    addTracks(result.tracks);
    if (dest?.object_id) objectMethod.set(dest.object_id, "files");
    await refreshRisk();
    if (result.tracks.length) {
      fitAll(result.tracks.map((t) => t.bounds).filter(Boolean));
      selectTrack(result.tracks[0].id);
    }
    const destLabel = targetLabel(dest);
    els.statTime.textContent = result.elapsed_ms ? `${result.elapsed_ms} ms` : "—";
    els.progress.textContent = result.errors.length
      ? `${result.errors.length} file(s) skipped`
      : result.tracks.length
        ? `Loaded ${result.tracks.length} exclusive traj → ${destLabel}`
        : "No trajectories loaded — pick CSV/text files, or a folder of them.";
    if (result.errors.length) {
      console.warn(result.errors);
    }
    await refreshMission();
  } catch (err) {
    els.progress.textContent = String(err);
  } finally {
    setBusy(false);
    render();
  }
}

function targetLabel(target) {
  const obj = riskModel.objects.find((o) => o.id === target?.object_id);
  if (!obj) return "";
  if (target.mode_name) return `${obj.name} / ${target.mode_name}`;
  const mode = obj.modes.find((m) => m.id === target.failure_mode_id);
  return mode ? `${obj.name} / ${mode.name}` : obj.name;
}

function setBusy(busy) {
  for (const btn of [els.addObject, els.missionNew, els.missionOpen, els.missionSave, els.loadBoats, els.sampleBoats, els.clearBoats]) {
    if (btn) btn.disabled = busy;
  }
  els.objectList?.querySelectorAll("button").forEach((btn) => {
    btn.disabled = busy;
  });
  document.querySelectorAll(".method-choice [data-act], .method-choice button").forEach((btn) => {
    btn.disabled = busy;
  });
  els.rocketpyOverlay?.querySelectorAll("button").forEach((btn) => {
    if (btn.dataset.act === "rp-close") return;
    btn.disabled = busy;
  });
}

function render() {
  const all = [...tracks.values()];
  if (els.statTracks) els.statTracks.textContent = String(all.length);
  if (els.statPoints) els.statPoints.textContent = formatCount(all.reduce((n, t) => n + t.point_count, 0));
  try {
    renderRisk();
    renderBoats();
  } catch (err) {
    console.error(err);
    if (els.progress) els.progress.textContent = String(err);
  }
}

function applyBoatResult(result, fly) {
  applyScoredBoats(result?.boats || []);
  if (fly && boats.length) {
    const bounds = boatBounds(boats.filter((b) => b.visible !== false));
    if (bounds) flyToBounds(bounds);
  }
  rescoreBoats();
}

function applyScoredBoats(list) {
  boats = (list || []).map((boat) => {
    const next = enrichBoat(boat);
    next.display_color = displayBoatColor(next);
    return next;
  });
  setBoats(boats);
  setBoatsVisible(els.showBoats?.checked !== false);
  updateBoatInspect(hoveredBoatId ?? selectedBoatId);
  renderBoats();
}

async function rescoreBoats() {
  try {
    const scored = await invoke("score_boats");
    applyScoredBoats(scored);
  } catch {
    applyScoredBoats(scoreBoatsAgainstGrid(boats, lastKdeGrid || currentKdeGrid()));
  }
}

function clearBoatScores() {
  lastKdeGrid = null;
  applyScoredBoats(boats.map((boat) => ({
    ...boat,
    area_m2: null,
    kde_density: null,
    p_hit: null,
    p_hit_mean: null,
    expected_casualties: null,
  })));
}

function displayBoatColor(boat) {
  const p = Number(boat.p_hit);
  if (!Number.isFinite(p) || p <= 0) return boat.color || "#3ecfc4";
  if (p < 1e-6) return "#5fd0c0";
  if (p < 1e-4) return "#d4c05a";
  if (p < 1e-2) return "#e08a3c";
  return "#e35d6a";
}

function boatRiskClass(boat) {
  const p = Number(boat.p_hit);
  if (!Number.isFinite(p) || p <= 0) return "";
  if (p >= 1e-2) return " risk-high";
  if (p >= 1e-4) return " risk-mid";
  return "";
}

function renderBoats() {
  if (els.boatRisk) {
    const scored = boats.filter((b) => b.p_hit != null);
    const ec = scored.reduce((n, b) => n + (Number(b.expected_casualties) || 0), 0);
    els.boatRisk.textContent = scored.length
      ? `${boats.length} boats · Σ Ec ${fmtRisk(ec)} against debris KDE`
      : boats.length
        ? `${boats.length} boats · load trajectories to score against the KDE`
        : "";
  }
  if (!els.boatList) return;
  if (!boats.length) {
    els.boatList.innerHTML = `<div class="empty">No boats loaded.</div>`;
    updateBoatInspect(null);
    return;
  }
  const ordered = [...boats].sort((a, b) => (Number(b.expected_casualties) || -1) - (Number(a.expected_casualties) || -1));
  els.boatList.innerHTML = "";
  for (const boat of ordered) {
    const row = document.createElement("div");
    const active = boat.id === selectedBoatId || boat.id === hoveredBoatId;
    row.className = `boat-row${boat.id === selectedBoatId ? " selected" : ""}${boatRiskClass(boat)}`;
    row.dataset.boatId = String(boat.id);
    row.innerHTML = `
      <span class="swatch" style="background:${escapeHtml(boat.display_color || boat.color || "#3ecfc4")}"></span>
      <div>
        <div class="boat-name">${escapeHtml(boat.name)}</div>
        <div class="boat-meta">${boatSummary(boat)}</div>
      </div>
      <div class="ops">
        <button type="button" data-act="vis">${boat.visible === false ? "Show" : "Hide"}</button>
        <button type="button" data-act="fly">Fly</button>
        <button type="button" data-act="del">✕</button>
      </div>`;
    row.addEventListener("click", (ev) => {
      if (ev.target.closest("button")) return;
      selectBoat(boat.id);
    });
    row.querySelector("[data-act='vis']").addEventListener("click", (ev) => {
      ev.stopPropagation();
      toggleBoatVisible(boat.id);
    });
    row.querySelector("[data-act='fly']").addEventListener("click", (ev) => {
      ev.stopPropagation();
      selectBoat(boat.id);
      const bounds = boatBounds([boat]);
      if (bounds) flyToBounds(bounds);
    });
    row.querySelector("[data-act='del']").addEventListener("click", (ev) => {
      ev.stopPropagation();
      deleteBoat(boat.id);
    });
    if (active) row.classList.add("selected");
    els.boatList.appendChild(row);
  }
}

function boatSummary(boat) {
  const bits = [];
  bits.push(`${fmtCoord(boat.lat)}, ${fmtCoord(boat.lon)}`);
  if (boat.speed_kn != null) bits.push(`${Number(boat.speed_kn).toFixed(1)} kn`);
  if (boat.heading_deg != null) bits.push(`${Math.round(Number(boat.heading_deg))}°`);
  if (boat.people_on_board != null) bits.push(`${boat.people_on_board} POB`);
  if (boat.length_m != null) bits.push(`${Number(boat.length_m).toFixed(0)} m`);
  if (boat.age_s != null) bits.push(`${fmtAge(boat.age_s)} stale`);
  if (boat.p_hit != null) bits.push(`P(hit) ${fmtRisk(boat.p_hit)}`);
  if (boat.expected_casualties != null) bits.push(`Ec ${fmtRisk(boat.expected_casualties)}`);
  return bits.join(" · ");
}

function fmtCoord(v) {
  return Number.isFinite(Number(v)) ? `${Number(v).toFixed(3)}°` : "—";
}

function fmtAge(age) {
  const s = Number(age);
  if (!Number.isFinite(s)) return "—";
  if (s < 90) return `${Math.round(s)} s`;
  if (s < 3600) return `${Math.round(s / 60)} min`;
  return `${(s / 3600).toFixed(1)} h`;
}

function selectBoat(id) {
  selectedBoatId = id;
  hoveredBoatId = id;
  highlightBoat(id);
  updateBoatInspect(id);
  renderBoats();
}

function bindBoatListHover() {
  if (!els.boatList || els.boatList.dataset.hoverBound) return;
  els.boatList.dataset.hoverBound = "1";
  els.boatList.addEventListener("pointerover", (ev) => {
    const row = ev.target.closest(".boat-row");
    if (!row || !els.boatList.contains(row)) return;
    const id = Number(row.dataset.boatId);
    if (!Number.isFinite(id)) return;
    inspectBoat(id, false);
  });
  els.boatList.addEventListener("pointerleave", () => {
    inspectBoat(selectedBoatId, false);
  });
}

function inspectBoat(id, select) {
  if (select) selectedBoatId = id;
  if (!select && hoveredBoatId === id) return;
  hoveredBoatId = id;
  highlightBoat(id ?? selectedBoatId);
  updateBoatInspect(id ?? selectedBoatId);
  if (select) renderBoats();
}

function onGlobeBoatHover(id, position) {
  if (id !== hoveredBoatId) inspectBoat(id, false);
  placeBoatTooltip(id, position);
}

function boatById(id) {
  return boats.find((b) => b.id === id) || null;
}

function updateBoatInspect(id) {
  const boat = boatById(id);
  if (!els.boatDetail) return;
  if (!boat) {
    els.boatDetail.classList.add("hidden");
    els.boatDetail.innerHTML = "";
    hideBoatTooltip();
    return;
  }
  els.boatDetail.classList.remove("hidden");
  els.boatDetail.innerHTML = `
    <h3>${escapeHtml(boat.name)}</h3>
    <dl>
      <dt>Position</dt><dd>${fmtCoord(boat.lat)}, ${fmtCoord(boat.lon)}</dd>
      <dt>Est. now</dt><dd>${fmtCoord(boat.estimate_lat)}, ${fmtCoord(boat.estimate_lon)}</dd>
      <dt>Speed</dt><dd>${boat.speed_kn != null ? `${Number(boat.speed_kn).toFixed(1)} kn` : "—"}</dd>
      <dt>Heading</dt><dd>${boat.heading_deg != null ? `${Math.round(Number(boat.heading_deg))}°` : "—"}</dd>
      <dt>People on board</dt><dd>${boat.people_on_board != null ? boat.people_on_board : "—"}</dd>
      <dt>Boat size</dt><dd>${boat.length_m != null ? `${Number(boat.length_m).toFixed(1)} m` : "—"}</dd>
      <dt>Age</dt><dd>${boat.age_s != null ? fmtAge(boat.age_s) : "—"}</dd>
      <dt>P(hit)</dt><dd>${fmtRisk(boat.p_hit)} peak · ${fmtRisk(boat.p_hit_mean)} mean</dd>
      <dt>Expected casualties</dt><dd>${fmtRisk(boat.expected_casualties)}</dd>
    </dl>`;
}

function placeBoatTooltip(id, position) {
  if (!els.boatTooltip) return;
  const boat = boatById(id);
  if (!boat || !position) {
    hideBoatTooltip();
    return;
  }
  const globe = document.getElementById("globe");
  const rect = globe?.getBoundingClientRect();
  const panel = document.getElementById("globe-panel")?.getBoundingClientRect();
  const x = (rect ? rect.left : 0) + position.x - (panel?.left || 0);
  const y = (rect ? rect.top : 0) + position.y - (panel?.top || 0);
  els.boatTooltip.classList.remove("hidden");
  els.boatTooltip.style.left = `${x + 14}px`;
  els.boatTooltip.style.top = `${y + 14}px`;
  els.boatTooltip.innerHTML = `
    <strong>${escapeHtml(boat.name)}</strong>
    <span>${fmtCoord(boat.lat)}, ${fmtCoord(boat.lon)}</span>
    <span class="muted">${boat.speed_kn != null ? `${Number(boat.speed_kn).toFixed(1)} kn` : "—"} · ${boat.heading_deg != null ? `${Math.round(Number(boat.heading_deg))}°` : "—"}</span>
    <span class="muted">${boat.people_on_board != null ? `${boat.people_on_board} people` : "POB —"} · ${boat.length_m != null ? `${Number(boat.length_m).toFixed(0)} m` : "size —"} · age ${boat.age_s != null ? fmtAge(boat.age_s) : "—"}</span>
    <span class="muted">P(hit) ${fmtRisk(boat.p_hit)} · Ec ${fmtRisk(boat.expected_casualties)}</span>`;
}

function hideBoatTooltip() {
  els.boatTooltip?.classList.add("hidden");
}

function fmtRisk(p) {
  if (p == null || Number.isNaN(Number(p))) return "—";
  const n = Number(p);
  if (n === 0) return "0";
  if (n >= 0.01) return n.toFixed(3);
  return n.toExponential(2);
}

async function runLoadBoats() {
  setBusy(true);
  els.progress.textContent = "Opening boat KML…";
  try {
    const result = await invoke("load_boats");
    applyBoatResult(result, true);
    await refreshMission();
    els.statTime.textContent = result.elapsed_ms ? `${result.elapsed_ms} ms` : "—";
    els.progress.textContent = boatLoadMessage(result, "Loaded");
  } catch {
    els.boatKmlInput?.click();
    els.progress.textContent = "";
  } finally {
    setBusy(false);
    render();
  }
}

async function runSampleBoats() {
  setBusy(true);
  els.progress.textContent = "Loading sample boats…";
  try {
    const result = await invoke("load_sample_boats");
    applyBoatResult(result, true);
    await refreshMission();
    els.statTime.textContent = result.elapsed_ms ? `${result.elapsed_ms} ms` : "—";
    els.progress.textContent = boatLoadMessage(result, "Loaded sample");
  } catch {
    try {
      const added = await loadBundledSampleBoats();
      els.progress.textContent = added ? `Loaded sample ${added} boats` : "Sample boat KML not available";
    } catch (err) {
      els.progress.textContent = String(err);
    }
  } finally {
    setBusy(false);
    render();
  }
}

async function loadBundledSampleBoats() {
  const mods = import.meta.glob("../samples/boats/*.kml", { query: "?raw", import: "default" });
  const names = Object.keys(mods).sort();
  let added = 0;
  for (const path of names) {
    const text = await mods[path]();
    const parsed = parseBoatKml(text, path.split("/").pop());
    boats = [...boats, ...parsed];
    added += parsed.length;
  }
  applyScoredBoats(boats);
  if (added) {
    const bounds = boatBounds(boats.filter((b) => b.visible !== false));
    if (bounds) flyToBounds(bounds);
  }
  await rescoreBoats();
  return added;
}

async function runClearBoats() {
  try {
    boats = await invoke("clear_boats");
  } catch {
    boats = [];
  }
  selectedBoatId = null;
  hoveredBoatId = null;
  hideBoatTooltip();
  applyScoredBoats(boats);
  highlightBoat(null);
  await refreshMission();
  render();
  els.progress.textContent = "Boats cleared";
}

async function importBoatFiles(fileList) {
  const files = [...(fileList || [])];
  if (els.boatKmlInput) els.boatKmlInput.value = "";
  if (!files.length) return;
  setBusy(true);
  let added = 0;
  const errors = [];
  try {
    for (const file of files) {
      const text = await file.text();
      try {
        const result = await invoke("import_boats_kml", ipcArgs({ text, name: file.name }));
        boats = (result.boats || []).map(enrichBoat);
        added += result.added || 0;
      } catch {
        try {
          const parsed = parseBoatKml(text, file.name);
          boats = [...boats, ...parsed];
          added += parsed.length;
        } catch (err) {
          errors.push(`${file.name}: ${err}`);
        }
      }
    }
    applyScoredBoats(boats);
    if (added) {
      const bounds = boatBounds(boats.filter((b) => b.visible !== false));
      if (bounds) flyToBounds(bounds);
    }
    await rescoreBoats();
    await refreshMission();
    els.progress.textContent = errors.length
      ? `${errors.length} KML file(s) skipped`
      : `Loaded ${added} boat${added === 1 ? "" : "s"}`;
  } finally {
    setBusy(false);
    render();
  }
}

function boatLoadMessage(result, verb) {
  if (result?.errors?.length) return `${result.errors.length} KML file(s) skipped`;
  const n = result?.added ?? 0;
  if (!n) return "";
  return `${verb} ${n} boat${n === 1 ? "" : "s"}`;
}

async function toggleBoatVisible(id) {
  const boat = boats.find((b) => b.id === id);
  if (!boat) return;
  const visible = boat.visible === false;
  try {
    await invoke("set_boat_visible", ipcArgs({ id, visible }));
  } catch {
    /* previewed outside the Tauri shell */
  }
  boat.visible = visible;
  applyScoredBoats(boats);
  await refreshMission();
  render();
}

async function deleteBoat(id) {
  try {
    await invoke("remove_boat", ipcArgs({ id }));
  } catch {
    /* previewed outside the Tauri shell */
  }
  boats = boats.filter((b) => b.id !== id);
  if (selectedBoatId === id) selectedBoatId = null;
  if (hoveredBoatId === id) hoveredBoatId = selectedBoatId;
  applyScoredBoats(boats);
  highlightBoat(selectedBoatId);
  await refreshMission();
  render();
}

async function createObject() {
  const name = `Object ${(riskModel.objects?.length || 0) + 1}`;
  pendingFocus = { focusModeName: true };
  pendingExpandLast = true;
  await mutateRisk("create_object", { name, source: "files" });
}

function isObjectExpanded(id) {
  if (expandedObjectId === undefined) {
    return riskModel.objects[0]?.id === id;
  }
  return expandedObjectId === id;
}

function toggleObjectCard(id) {
  expandedObjectId = isObjectExpanded(id) ? null : id;
  render();
  if (expandedObjectId === id) revealCard(id);
}

function revealCard(id) {
  requestAnimationFrame(() => {
    const card = els.objectList?.querySelector(`[data-object-id="${id}"]`);
    card?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  });
}

function syncExpandedObject() {
  if (pendingExpandLast && riskModel.objects.length) {
    expandedObjectId = riskModel.objects[riskModel.objects.length - 1].id;
    pendingExpandLast = false;
  }
  if (expandedObjectId == null) return;
  if (!riskModel.objects.some((o) => o.id === expandedObjectId)) {
    expandedObjectId = riskModel.objects[0]?.id ?? null;
  }
}

function objectCardSummary(obj) {
  const n = obj.modes?.length || 0;
  const modes = n === 1 ? "1 mode" : `${n} modes`;
  const status = obj.valid ? "✓" : n ? "need 1" : "empty";
  return `${modes} · ${status}`;
}

function bindSimActions() {
  const host = els.objectList;
  if (!host || host.dataset.boundSim) return;
  host.dataset.boundSim = "1";
  host.addEventListener("click", (ev) => {
    const rm = ev.target.closest("[data-rm-time]");
    if (rm) {
      simDraft.nav.times.splice(Number(rm.dataset.rmTime), 1);
      render();
      return;
    }
    const act = ev.target.closest("[data-act]")?.dataset.act;
    if (!act) return;
    const run = (fn) => {
      Promise.resolve(fn()).catch((err) => {
        if (els.progress) els.progress.textContent = String(err);
      });
    };
    if (act === "run-stage") run(runSimStage);
    else if (act === "run-fts") run(runSimFts);
    else if (act === "run-nav") run(runSimNav);
    else if (act === "add-time") run(addNavTime);
    else if (act === "fill-times") run(async () => {
      await fillNavTimes();
      render();
    });
    else if (act === "toggle-cat") {
      simDraft.showCatalog = !simDraft.showCatalog;
      render();
    }
  });
}

async function addNavTime() {
  readSimForm();
  if (!simDraft.trackId) simDraft.trackId = preferredSimTrackId();
  if (!simDraft.sample || simDraft.sample.track_id !== simDraft.trackId) {
    await refreshSimSample(false);
  }
  const sliderTime = Number(simHost()?.querySelector("[data-sim='time']")?.value);
  const tNow = simDraft.sample?.time_s ?? simDraft.timeS ?? (Number.isFinite(sliderTime) ? sliderTime : null);
  if (tNow == null || Number.isNaN(Number(tNow))) {
    els.progress.textContent = "Move the time slider, then Add current time.";
    return;
  }
  const alt = Number(simDraft.sample?.alt_m);
  if (Number.isFinite(alt) && alt < 80) {
    els.progress.textContent = `t = ${Number(tNow).toFixed(1)} s is at ${alt.toFixed(0)} m — pick an in-flight time.`;
    return;
  }
  const next = Number(Number(tNow).toFixed(3));
  if (!simDraft.nav.times.some((x) => Math.abs(x - next) < 1e-6)) {
    simDraft.nav.times.push(next);
    simDraft.nav.times.sort((a, b) => a - b);
  }
  render();
}

function bindSectionToggles() {
  document.getElementById("toggle-wind")?.addEventListener("click", () => {
    sectionCollapsed.wind = !sectionCollapsed.wind;
    applySectionCollapsed();
  });
  document.getElementById("toggle-boats")?.addEventListener("click", () => {
    sectionCollapsed.boats = !sectionCollapsed.boats;
    applySectionCollapsed();
  });
  applySectionCollapsed();
}

function applySectionCollapsed() {
  const wind = document.getElementById("wind-panel");
  const boats = document.getElementById("boats-panel");
  const tw = document.getElementById("toggle-wind");
  const tb = document.getElementById("toggle-boats");
  wind?.classList.toggle("collapsed", sectionCollapsed.wind);
  boats?.classList.toggle("collapsed", sectionCollapsed.boats);
  tw?.setAttribute("aria-expanded", String(!sectionCollapsed.wind));
  tb?.setAttribute("aria-expanded", String(!sectionCollapsed.boats));
}

function selectTrack(id) {
  selectedId = id;
  highlight(id);
  render();
}

async function toggleVisible(id) {
  const track = tracks.get(id);
  if (!track) return;
  await setTracksVisible([id], !track.visible);
}

async function setTracksVisible(ids, visible) {
  for (const id of ids) {
    const track = tracks.get(id);
    if (!track || track.visible === visible) continue;
    track.visible = visible;
    await invoke("set_visible", { id, visible });
    setVisible(id, visible);
  }
  await refreshMission();
  await refreshOverlays({ force: true, immediate: true });
  render();
}

async function deleteTrack(id) {
  await discardTracks([id]);
}

function formatCount(n) {
  return n.toLocaleString();
}

function formatProb(p) {
  if (p == null || Number.isNaN(p)) return "—";
  if (p === 0) return "0";
  if (p >= 0.01) return p.toFixed(3);
  return p.toExponential(2);
}

function ipcArgs(args) {
  const out = { ...args };
  for (const [key, value] of Object.entries(args)) {
    if (!key.includes("_")) {
      const snake = key.replace(/[A-Z]/g, (ch) => `_${ch.toLowerCase()}`);
      if (snake !== key && !(snake in out)) out[snake] = value;
    }
  }
  return out;
}

function suggestedModeName(obj) {
  const used = new Set((obj.modes || []).map((m) => m.name.trim().toLowerCase()));
  for (const candidate of ["Nominal", "Failure", "Abort", "Staging"]) {
    if (!used.has(candidate.toLowerCase())) return candidate;
  }
  return `Mode ${(obj.modes?.length || 0) + 1}`;
}

function fieldText(el) {
  const v = el?.value;
  return typeof v === "string" ? v.trim() : "";
}

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[ch]));
}

async function refreshRisk(opts = {}) {
  try {
    riskModel = await invoke("get_risk_model");
  } catch {
    riskModel = { objects: [], unassigned: [], wind: { type: "off" } };
  }
  applyMissionWind(riskModel.wind);
  syncCatalogs(riskModel);
  applyRiskToTracks();
  syncTracksToModel();
  await refreshOverlays(opts);
}

function syncTracksToModel() {
  const keep = new Set();
  for (const obj of riskModel.objects || []) {
    for (const mode of obj.modes || []) {
      for (const t of mode.tracks || []) keep.add(t.id);
    }
  }
  for (const t of riskModel.unassigned || []) keep.add(t.id);
  for (const id of [...tracks.keys()]) {
    if (keep.has(id)) continue;
    tracks.delete(id);
    removeTrack(id);
    if (selectedId === id) selectedId = null;
  }
}

function applyRiskToTracks() {
  for (const obj of riskModel.objects || []) {
    for (const mode of obj.modes || []) {
      for (const t of mode.tracks || []) {
        const track = tracks.get(t.id);
        if (!track) continue;
        track.object_id = obj.id;
        track.failure_mode_id = mode.id;
        track.weight = t.weight;
        track.probability = t.probability;
      }
    }
  }
  for (const t of riskModel.unassigned || []) {
    const track = tracks.get(t.id);
    if (!track) continue;
    track.object_id = t.object_id;
    track.failure_mode_id = t.failure_mode_id;
    track.probability = 0;
  }
}

async function mutateRisk(cmd, args) {
  try {
    riskModel = await invoke(cmd, ipcArgs(args));
    applyRiskToTracks();
    syncTracksToModel();
    await refreshOverlays({ force: true, immediate: true });
    await refreshMission();
    render();
    applyPendingFocus();
    return true;
  } catch (err) {
    pendingFocus = null;
    els.progress.textContent = String(err);
    return false;
  }
}

function applyPendingFocus() {
  if (!pendingFocus) return;
  let card = null;
  if (pendingFocus.focusModeName) {
    const cards = els.objectList?.querySelectorAll("[data-object-id]");
    card = cards?.[cards.length - 1] || null;
    const input = card?.querySelector('[data-role="load-mode-name"]');
    input?.focus();
    input?.select();
  } else if (pendingFocus.focusObjectName) {
    const cards = els.objectList?.querySelectorAll("[data-object-id]");
    card = cards?.[cards.length - 1] || null;
    const input = card?.querySelector('[data-role="obj-name"]');
    input?.focus();
    input?.select();
  } else {
    card = els.objectList?.querySelector(`[data-object-id="${pendingFocus.objectId}"]`);
    const names = card?.querySelectorAll('[data-role="mode-name"]');
    const last = names?.[names.length - 1];
    last?.focus();
    last?.select();
  }
  pendingFocus = null;
  const id = Number(card?.dataset.objectId);
  if (Number.isFinite(id)) revealCard(id);
}

function renderRisk() {
  if (!riskModel.objects) riskModel.objects = [];
  if (!riskModel.unassigned) riskModel.unassigned = [];
  if (!els.objectList) return;
  syncExpandedObject();
  els.objectList.innerHTML = "";
  if (!riskModel.objects.length) {
    const recent = (mission.recent || [])
      .map(
        (item) =>
          `<button type="button" class="ghost" data-recent-path="${escapeHtml(item.path)}">${escapeHtml(item.name)}<br><span class="muted">${escapeHtml(item.path)}</span></button>`
      )
      .join("");
    els.objectList.innerHTML = `
      <div class="empty">Add an object, name a mode, then add trajectories: load CSVs, generate a flight, or branch from an existing state.</div>
      ${recent ? `<div class="recent-missions"><div class="muted">Recent missions</div>${recent}</div>` : ""}`;
    els.objectList.querySelectorAll("[data-recent-path]").forEach((btn) => {
      btn.addEventListener("click", () => openRecent(btn.dataset.recentPath));
    });
  }
  for (const obj of riskModel.objects) {
    const card = document.createElement("div");
    const expanded = isObjectExpanded(obj.id);
    card.className = `object-card ${obj.valid ? "ok" : "invalid"}${expanded ? "" : " collapsed"}`;
    card.dataset.objectId = String(obj.id);
    const sumClass = obj.valid ? "good" : "bad";
    card.innerHTML = `
      <div class="object-head">
        <button type="button" data-act="toggle-card" class="ghost compact card-toggle" aria-expanded="${expanded}" title="${expanded ? "Collapse card" : "Expand card"}">${expanded ? "▾" : "▸"}</button>
        <input type="text" data-role="obj-name" value="${escapeHtml(obj.name)}" />
        <span class="card-summary muted">${escapeHtml(objectCardSummary(obj))}</span>
        <button data-act="del-obj" class="ghost compact">✕</button>
      </div>
      <div class="card-body">
      <div class="prob-sum ${sumClass}">
        Inclusive object · modes Σ ${formatProb(obj.failure_mode_sum)} · traj Σ ${formatProb(obj.trajectory_prob_sum)}
        ${obj.valid ? "✓" : obj.modes.length ? "— need 1" : "— add a mode and trajectories"}
        ${obj.issues.length ? `<br>${escapeHtml(obj.issues.join(" · "))}` : ""}
      </div>
      <div class="method-choice"></div>
      <div class="modes"></div>
      </div>`;
    fillMethodChoice(card.querySelector(".method-choice"), obj);
    const loadMode = loadModeFor(obj);
    const method = methodFor(obj);
    const modesEl = card.querySelector(".modes");
    if (!obj.modes.length) {
      modesEl.innerHTML = `<div class="empty">No modes yet. Name a mode above and add trajectories with Load, Generate, or From state.</div>`;
    }
    for (const mode of obj.modes) {
      const block = document.createElement("div");
      const isLoadTarget = method === "files" && loadMode?.id === mode.id;
      const isGenTarget = method === "generate" && loadMode?.id === mode.id;
      const isSimTarget = method === "from-state" && loadMode?.id === mode.id;
      const isTarget = isLoadTarget || isGenTarget || isSimTarget;
      const destBadge = isLoadTarget
        ? '<span class="badge dest-badge">Load target</span>'
        : isGenTarget
          ? '<span class="badge dest-badge">Generate target</span>'
          : isSimTarget
            ? '<span class="badge dest-badge">From-state target</span>'
            : "";
      const destNote = isLoadTarget
        ? " · loaded files go here"
        : isGenTarget
          ? " · generate adds 1 traj here"
          : isSimTarget
            ? " · branched traj go here"
            : "";
      block.className = `mode${isTarget ? " mode-target" : ""}`;
      block.innerHTML = `
        <div class="mode-head">
          <input type="text" data-role="mode-name" value="${escapeHtml(mode.name)}" />
          <input type="number" data-role="mode-p" min="0" max="1" step="0.001" value="${mode.probability}" title="Mode probability" />
          ${destBadge}
          <button data-act="del-mode" class="ghost compact">✕</button>
        </div>
        <div class="muted">${mode.track_count} exclusive traj · Σ p = ${formatProb(mode.trajectory_prob_sum)}${destNote}</div>
        <div class="mode-tracks"></div>`;
      const list = block.querySelector(".mode-tracks");
      list.appendChild(trackGroup(mode.tracks, `${mode.track_count} exclusive traj`));
      block.querySelector('[data-role="mode-name"]').addEventListener("change", (ev) => {
        mutateRisk("update_failure_mode", { id: mode.id, name: ev.target.value });
      });
      block.querySelector('[data-role="mode-p"]').addEventListener("change", (ev) => {
        mutateRisk("update_failure_mode", { id: mode.id, probability: Number(ev.target.value) });
      });
      block.querySelector('[data-act="del-mode"]').addEventListener("click", () => {
        mutateRisk("remove_failure_mode", { id: mode.id });
      });
      modesEl.appendChild(block);
    }
    card.querySelector('[data-role="obj-name"]').addEventListener("change", (ev) => {
      mutateRisk("rename_object", { id: obj.id, name: ev.target.value });
    });
    card.querySelector('[data-act="del-obj"]').addEventListener("click", () => {
      mutateRisk("remove_object", { id: obj.id });
      generateDrafts.delete(obj.id);
      generateEngine.delete(obj.id);
      rocketpyDrafts.delete(obj.id);
      objectMethod.delete(obj.id);
      objectLoadName.delete(obj.id);
      if (rocketpyOpenId === obj.id) closeRocketPyBuilder({ render: false });
    });
    card.querySelector('[data-act="toggle-card"]').addEventListener("click", (ev) => {
      ev.stopPropagation();
      toggleObjectCard(obj.id);
    });
    card.querySelector(".object-head").addEventListener("click", (ev) => {
      if (ev.target.closest("input, [data-act='del-obj']")) return;
      toggleObjectCard(obj.id);
    });
    els.objectList.appendChild(card);
  }
  refreshSiteMarkers();

  const n = riskModel.unassigned.length;
  if (!els.unassigned) return;
  els.unassigned.innerHTML = "";
  if (n) {
    const dests = riskModel.objects.flatMap((o) =>
      (o.modes || []).map((m) => ({
        object_id: o.id,
        failure_mode_id: m.id,
        label: `${o.name} / ${m.name}`,
      }))
    );
    const wrap = document.createElement("div");
    wrap.className = "unassigned-box";
    wrap.innerHTML = `
      <div>${n} unassigned — assign into a mode or discard.</div>
      <div class="unassigned-ops">
        <select data-role="assign-dest" ${dests.length ? "" : "disabled"}>
          ${
            dests.length
              ? dests.map((d) => `<option value="${d.object_id}:${d.failure_mode_id}">${escapeHtml(d.label)}</option>`).join("")
              : `<option>Add an object first</option>`
          }
        </select>
        <button type="button" data-act="assign-one" class="compact" ${dests.length ? "" : "disabled"}>Assign selected</button>
        <button type="button" data-act="assign-all" class="compact" ${dests.length ? "" : "disabled"}>Assign all</button>
        <button type="button" data-act="discard-all" class="ghost compact">Discard all</button>
      </div>`;
    const picker = trackGroup(riskModel.unassigned, `${n} unassigned`);
    wrap.appendChild(picker);
    const destSelect = wrap.querySelector("[data-role='assign-dest']");
    const parseDest = () => {
      const [objectId, failureModeId] = String(destSelect.value).split(":").map(Number);
      return { objectId, failureModeId };
    };
    wrap.querySelector("[data-act='assign-one']").addEventListener("click", () => {
      const pick = picker.querySelector("[data-role='track-pick']");
      const id = Number(pick?.value);
      const ids = id
        ? [id]
        : String(picker.dataset.trackIds || "")
            .split(",")
            .map(Number)
            .filter(Boolean);
      if (!ids.length) {
        els.progress.textContent = "Select an unassigned trajectory, then Assign selected.";
        return;
      }
      assignUnassigned(ids, parseDest());
    });
    wrap.querySelector("[data-act='assign-all']").addEventListener("click", () => {
      assignUnassigned(riskModel.unassigned.map((t) => t.id), parseDest());
    });
    wrap.querySelector("[data-act='discard-all']").addEventListener("click", () => {
      discardTracks(riskModel.unassigned.map((t) => t.id));
    });
    els.unassigned.appendChild(wrap);
  }
}

async function assignUnassigned(trackIds, dest) {
  if (!dest?.objectId || !dest?.failureModeId || !trackIds.length) {
    els.progress.textContent = "Add an object, then assign into one of its modes.";
    return;
  }
  await mutateRisk("assign_tracks", {
    trackIds,
    objectId: dest.objectId,
    failureModeId: dest.failureModeId,
  });
}

async function discardTracks(ids) {
  const unique = [...new Set((ids || []).filter((id) => Number(id)))];
  if (!unique.length) return;
  try {
    await invoke("remove_tracks", ipcArgs({ ids: unique }));
  } catch {
    for (const id of unique) {
      try {
        await invoke("remove_track", { id });
      } catch {
        /* already gone */
      }
    }
  }
  for (const id of unique) {
    tracks.delete(id);
    removeTrack(id);
    if (selectedId === id) selectedId = null;
  }
  await refreshRisk({ force: true, immediate: true });
  await refreshMission();
  render();
}

function trackHasPath(t) {
  if (!t) return false;
  if (t.show_path === false) return false;
  const n = Number(t.display_count);
  if (Number.isFinite(n) && n > 0) return n >= 2;
  return (t.display_lla?.length || 0) >= 6;
}

function itemHasPath(item) {
  const t = tracks.get(item.id);
  if (!t) return true;
  return trackHasPath(t);
}

function trackGroup(items, groupLabel) {
  const wrap = document.createElement("div");
  if (!items.length) {
    wrap.innerHTML = `<div class="empty">No files in this mode yet.</div>`;
    return wrap;
  }
  const pathItems = items.filter(itemHasPath);
  const impactItems = items.filter((item) => !itemHasPath(item));
  const ids = items.map((item) => item.id);
  wrap.dataset.trackIds = ids.join(",");
  const anyVisible = ids.some((id) => tracks.get(id)?.visible !== false);

  if (!pathItems.length) {
    const sample = tracks.get(impactItems[0].id);
    wrap.className = `track track-group${items.some((item) => item.id === selectedId) ? " selected" : ""}`;
    wrap.innerHTML = `
      <span class="swatch" style="background:${sample?.color || "#6d7b88"}"></span>
      <span class="debris-label">${impactItems.length} debris impacts</span>
      <span class="prob">points + KDE</span>
      <div class="ops">
        <button type="button" data-act="vis-all">${anyVisible ? "Hide all" : "Show all"}</button>
        <button type="button" data-act="fly">Fly</button>
        <button type="button" data-act="del">✕</button>
      </div>`;
    wrap.querySelector("[data-act='vis-all']").addEventListener("click", (ev) => {
      ev.stopPropagation();
      setTracksVisible(ids, !anyVisible);
    });
    wrap.querySelector("[data-act='fly']").addEventListener("click", (ev) => {
      ev.stopPropagation();
      flyToImpactsOrTracks(impactItems.map((item) => tracks.get(item.id)).filter(Boolean));
    });
    wrap.querySelector("[data-act='del']").addEventListener("click", (ev) => {
      ev.stopPropagation();
      discardTracks(ids);
    });
    return wrap;
  }

  const listed = pathItems;
  const activeId = listed.some((item) => item.id === selectedId) ? selectedId : listed[0].id;
  const active = items.find((item) => item.id === activeId) || listed[0];
  const track = tracks.get(activeId);
  wrap.className = `track track-group${activeId === selectedId ? " selected" : ""}`;
  const options = listed
    .map((item) => {
      const name = tracks.get(item.id)?.name || item.name;
      const selected = item.id === activeId ? " selected" : "";
      return `<option value="${item.id}"${selected}>${escapeHtml(name)}</option>`;
    })
    .join("");
  wrap.innerHTML = `
    <span class="swatch" style="background:${track?.color || "#6d7b88"}"></span>
    <select data-role="track-pick" title="${escapeHtml(groupLabel)}">
      <optgroup label="${escapeHtml(groupLabel)}">${options}</optgroup>
    </select>
    <span class="prob">p=${formatProb(active?.probability)}</span>
    <div class="ops">
      <button type="button" data-act="vis-all">${anyVisible ? "Hide all" : "Show all"}</button>
      <button type="button" data-act="vis">${track?.visible === false ? "Show" : "Hide"}</button>
      <button type="button" data-act="fly">Fly</button>
      <button type="button" data-act="del">✕</button>
    </div>
    ${impactItems.length ? `<div class="muted debris-note">${impactItems.length} debris impacts (points + KDE) <button type="button" data-act="del-debris" class="ghost compact">Clear</button></div>` : ""}`;
  const pick = wrap.querySelector("[data-role='track-pick']");
  const currentId = () => Number(pick.value);
  pick.addEventListener("change", () => selectTrack(currentId()));
  wrap.querySelector("[data-act='vis-all']").addEventListener("click", (ev) => {
    ev.stopPropagation();
    setTracksVisible(ids, !anyVisible);
  });
  wrap.querySelector("[data-act='vis']").addEventListener("click", (ev) => {
    ev.stopPropagation();
    toggleVisible(currentId());
  });
  wrap.querySelector("[data-act='fly']").addEventListener("click", (ev) => {
    ev.stopPropagation();
    const chosen = tracks.get(currentId());
    if (chosen?.bounds) flyToBounds(chosen.bounds);
    selectTrack(currentId());
  });
  wrap.querySelector("[data-act='del']").addEventListener("click", (ev) => {
    ev.stopPropagation();
    if (impactItems.length) discardTracks(ids);
    else deleteTrack(currentId());
  });
  wrap.querySelector("[data-act='del-debris']")?.addEventListener("click", (ev) => {
    ev.stopPropagation();
    discardTracks(impactItems.map((item) => item.id));
  });
  wrap.addEventListener("click", (ev) => {
    if (ev.target.closest("select, button")) return;
    selectTrack(currentId());
  });
  return wrap;
}

function defaultWindDate() {
  const d = new Date();
  d.setUTCDate(d.getUTCDate() - 7);
  return d.toISOString().slice(0, 10);
}

function draftFor(obj) {
  if (!generateDrafts.has(obj.id)) {
    const g = obj.generate || {};
    generateDrafts.set(obj.id, {
      launch_lat: g.launch_lat ?? "",
      launch_lon: g.launch_lon ?? "",
      launch_alt_m: g.launch_alt_m ?? 0,
      aim_lat: g.aim_lat ?? "",
      aim_lon: g.aim_lon ?? "",
      aim_alt_m: g.aim_alt_m ?? 0,
      ballistic_coeff: g.ballistic_coeff ?? 2500,
      burnout_alt_m: g.burnout_alt_m ?? 80000,
      failure_count: g.failure_count ?? 8,
    });
  }
  return generateDrafts.get(obj.id);
}

function defaultRocketPySpec() {
  return {
    env: {
      latitude: 32.990254,
      longitude: -106.974998,
      elevation_m: 1400,
      atmosphere: "standard_atmosphere",
      wind_speed_mps: 0,
      wind_from_deg: 270,
    },
    motor: {
      thrust_n: 1500,
      burn_time_s: 3.9,
      dry_mass_kg: 1.815,
      propellant_mass_kg: 2.5,
      dry_inertia: [0.125, 0.125, 0.002],
      nozzle_radius_m: 0.033,
      chamber_radius_m: 0.033,
      chamber_height_m: 0.6,
      nozzle_position_m: 0,
      chamber_position_m: 0,
    },
    rocket: {
      radius_m: 0.0635,
      mass_kg: 14.426,
      inertia: [6.321, 6.321, 0.034],
      power_off_cd: 0.5,
      power_on_cd: 0.5,
      center_of_mass_m: 0,
      motor_position_m: -1.255,
      nose_length_m: 0.55829,
      nose_kind: "von karman",
      nose_position_m: 1.278,
      fin_n: 4,
      fin_root_m: 0.12,
      fin_tip_m: 0.06,
      fin_span_m: 0.11,
      fin_position_m: -1.04956,
      tail_top_m: 0.0635,
      tail_bottom_m: 0.0435,
      tail_length_m: 0.06,
      tail_position_m: -1.194656,
      rail_upper_m: 0.0818,
      rail_lower_m: -0.618,
    },
    flight: {
      rail_length_m: 5.2,
      inclination_deg: 85,
      heading_deg: 0,
      max_time_s: 400,
    },
  };
}

function soundingRocketPySpec() {
  const spec = defaultRocketPySpec();
  spec.env.elevation_m = 0;
  spec.motor.thrust_n = 8000;
  spec.motor.burn_time_s = 12;
  spec.motor.dry_mass_kg = 12;
  spec.motor.propellant_mass_kg = 40;
  spec.motor.chamber_height_m = 1.4;
  spec.rocket.mass_kg = 80;
  spec.rocket.radius_m = 0.1;
  spec.rocket.inertia = [40, 40, 0.2];
  spec.rocket.nose_length_m = 0.8;
  spec.rocket.fin_span_m = 0.18;
  spec.flight.rail_length_m = 12;
  spec.flight.max_time_s = 600;
  return spec;
}

function mergeRocketPySpec(saved) {
  const base = defaultRocketPySpec();
  if (!saved) return base;
  return {
    env: { ...base.env, ...(saved.env || {}) },
    motor: {
      ...base.motor,
      ...(saved.motor || {}),
      dry_inertia: Array.isArray(saved.motor?.dry_inertia) ? saved.motor.dry_inertia.slice() : base.motor.dry_inertia,
    },
    rocket: {
      ...base.rocket,
      ...(saved.rocket || {}),
      inertia: Array.isArray(saved.rocket?.inertia) ? saved.rocket.inertia.slice() : base.rocket.inertia,
    },
    flight: { ...base.flight, ...(saved.flight || {}) },
  };
}

function rocketpyDraftFor(obj) {
  if (!rocketpyDrafts.has(obj.id)) {
    rocketpyDrafts.set(obj.id, mergeRocketPySpec(obj.rocketpy));
  }
  return rocketpyDrafts.get(obj.id);
}

function rocketpySummary(draft) {
  const lat = Number(draft?.env?.latitude);
  const lon = Number(draft?.env?.longitude);
  const site = Number.isFinite(lat) && Number.isFinite(lon)
    ? `${lat.toFixed(3)}°, ${lon.toFixed(3)}°`
    : "no launch";
  return `${site} · ${draft.motor.thrust_n} N / ${draft.motor.burn_time_s} s · rail ${draft.flight.inclination_deg}° / ${draft.flight.heading_deg}°`;
}

function getRpPath(obj, path) {
  return path.split(".").reduce((cur, key) => (cur == null ? cur : cur[key]), obj);
}

function setRpPath(obj, path, value) {
  const parts = path.split(".");
  let cur = obj;
  for (let i = 0; i < parts.length - 1; i++) {
    const key = /^\d+$/.test(parts[i]) ? Number(parts[i]) : parts[i];
    if (cur[key] == null) cur[key] = /^\d+$/.test(parts[i + 1]) ? [] : {};
    cur = cur[key];
  }
  const last = parts[parts.length - 1];
  cur[/^\d+$/.test(last) ? Number(last) : last] = value;
}

function rpField(path, label, opts = {}) {
  const draft = rocketpyDrafts.get(rocketpyOpenId);
  const raw = getRpPath(draft, path);
  const value = raw == null ? "" : raw;
  const step = opts.step ?? "any";
  const min = opts.min != null ? ` min="${opts.min}"` : "";
  const max = opts.max != null ? ` max="${opts.max}"` : "";
  if (opts.select) {
    const options = opts.select.map((item) => {
      const [val, text] = Array.isArray(item) ? item : [item, item];
      return `<option value="${escapeHtml(val)}" ${String(val) === String(value) ? "selected" : ""}>${escapeHtml(text)}</option>`;
    }).join("");
    return `<label>${escapeHtml(label)} <select data-rp="${escapeHtml(path)}">${options}</select></label>`;
  }
  return `<label>${escapeHtml(label)} <input type="number" data-rp="${escapeHtml(path)}" value="${escapeHtml(value)}" step="${step}"${min}${max} /></label>`;
}

function rocketpyTabHtml(tab) {
  if (tab === "motor") {
    return `
      <div class="rp-note">Constant-thrust GenericMotor. Curve files can come later — these numbers are enough to fly.</div>
      <div class="rp-grid">
        ${rpField("motor.thrust_n", "Thrust N", { min: 1, step: 10 })}
        ${rpField("motor.burn_time_s", "Burn s", { min: 0.05, step: 0.1 })}
        ${rpField("motor.propellant_mass_kg", "Propellant kg", { min: 0.01, step: 0.01 })}
        ${rpField("motor.dry_mass_kg", "Dry mass kg", { min: 0.01, step: 0.01 })}
        ${rpField("motor.nozzle_radius_m", "Nozzle r m", { min: 0.001, step: 0.001 })}
        ${rpField("motor.chamber_radius_m", "Chamber r m", { min: 0.001, step: 0.001 })}
        ${rpField("motor.chamber_height_m", "Chamber h m", { min: 0.01, step: 0.01 })}
        ${rpField("motor.dry_inertia.0", "Ixx dry", { step: 0.001 })}
        ${rpField("motor.dry_inertia.1", "Iyy dry", { step: 0.001 })}
        ${rpField("motor.dry_inertia.2", "Izz dry", { step: 0.001 })}
      </div>`;
  }
  if (tab === "rocket") {
    return `
      <div class="rp-note">Positions are metres along the rocket, tail-to-nose. Calisto defaults match the 6DOF example.</div>
      <div class="rp-grid">
        ${rpField("rocket.radius_m", "Radius m", { min: 0.01, step: 0.001 })}
        ${rpField("rocket.mass_kg", "Dry mass kg", { min: 0.1, step: 0.01 })}
        ${rpField("rocket.power_off_cd", "Cd power-off", { min: 0.05, step: 0.01 })}
        ${rpField("rocket.power_on_cd", "Cd power-on", { min: 0.05, step: 0.01 })}
        ${rpField("rocket.center_of_mass_m", "CM w/o motor m", { step: 0.001 })}
        ${rpField("rocket.motor_position_m", "Motor pos. m", { step: 0.001 })}
        ${rpField("rocket.inertia.0", "Ixx", { step: 0.001 })}
        ${rpField("rocket.inertia.1", "Iyy", { step: 0.001 })}
        ${rpField("rocket.inertia.2", "Izz", { step: 0.001 })}
      </div>
      <div class="muted method-label">Nose, fins, tail</div>
      <div class="rp-grid">
        ${rpField("rocket.nose_length_m", "Nose length m", { min: 0.01, step: 0.001 })}
        ${rpField("rocket.nose_kind", "Nose kind", { select: ["von karman", "ogive", "conical", "lvhaack"] })}
        ${rpField("rocket.nose_position_m", "Nose pos. m", { step: 0.001 })}
        ${rpField("rocket.fin_n", "Fins", { min: 3, max: 8, step: 1 })}
        ${rpField("rocket.fin_root_m", "Root chord m", { min: 0.01, step: 0.001 })}
        ${rpField("rocket.fin_tip_m", "Tip chord m", { min: 0.01, step: 0.001 })}
        ${rpField("rocket.fin_span_m", "Span m", { min: 0.01, step: 0.001 })}
        ${rpField("rocket.fin_position_m", "Fin pos. m", { step: 0.001 })}
        ${rpField("rocket.tail_length_m", "Tail length m", { min: 0, step: 0.001 })}
        ${rpField("rocket.tail_top_m", "Tail top r m", { min: 0, step: 0.001 })}
        ${rpField("rocket.tail_bottom_m", "Tail bot r m", { min: 0, step: 0.001 })}
        ${rpField("rocket.tail_position_m", "Tail pos. m", { step: 0.001 })}
        ${rpField("rocket.rail_upper_m", "Rail btn upper m", { step: 0.001 })}
        ${rpField("rocket.rail_lower_m", "Rail btn lower m", { step: 0.001 })}
      </div>`;
  }
  if (tab === "flight") {
    return `
      <div class="rp-note">Inclination 90° is vertical. Heading is degrees from north. Click the globe (right of this panel) to set launch.</div>
      <div class="rp-grid cols-2">
        ${rpField("flight.rail_length_m", "Rail length m", { min: 0.5, step: 0.1 })}
        ${rpField("flight.inclination_deg", "Inclination °", { min: 0, max: 90, step: 0.1 })}
        ${rpField("flight.heading_deg", "Heading °", { min: 0, max: 360, step: 1 })}
        ${rpField("flight.max_time_s", "Max time s", { min: 10, max: 2000, step: 10 })}
      </div>`;
  }
  return `
    <div class="rp-note">Launch site for the 6DOF environment. Atmosphere is US Standard unless you add wind.</div>
    <div class="rp-grid">
      ${rpField("env.latitude", "Latitude", { min: -90, max: 90, step: 0.0001 })}
      ${rpField("env.longitude", "Longitude", { min: -180, max: 180, step: 0.0001 })}
      ${rpField("env.elevation_m", "Elevation m", { step: 1 })}
      ${rpField("env.atmosphere", "Atmosphere", { select: [["standard_atmosphere", "Standard"], ["isa", "ISA"]] })}
      ${rpField("env.wind_speed_mps", "Wind m/s", { min: 0, step: 0.5 })}
      ${rpField("env.wind_from_deg", "Wind from °", { min: 0, max: 360, step: 1 })}
    </div>
    <button type="button" data-act="rp-pick-launch" class="ghost compact">Pick launch on globe · ${fmtSite(rocketpyDrafts.get(rocketpyOpenId)?.env?.latitude, rocketpyDrafts.get(rocketpyOpenId)?.env?.longitude)}</button>`;
}

function paintRocketPyOverlay() {
  const overlay = els.rocketpyOverlay;
  if (!overlay || rocketpyOpenId == null) return;
  const obj = riskModel.objects.find((o) => o.id === rocketpyOpenId);
  const draft = obj ? rocketpyDraftFor(obj) : rocketpyDrafts.get(rocketpyOpenId);
  if (!draft) return;
  const name = obj?.name || "object";
  const tabs = [
    ["env", "Environment"],
    ["motor", "Motor"],
    ["rocket", "Rocket"],
    ["flight", "Flight"],
  ];
  const status = rocketpyStatus.available
    ? `6DOF ${rocketpyStatus.version || ""} · ${rocketpyStatus.python || "python"}`.trim()
    : (rocketpyStatus.error || "Checking Python…");
  overlay.innerHTML = `
    <div class="rp-head">
      <h2>6DOF · ${escapeHtml(name)}</h2>
      <button type="button" data-act="rp-close" class="ghost compact" title="Close builder">✕</button>
    </div>
    <div class="rp-tabs" role="tablist">
      ${tabs.map(([id, label]) => `<button type="button" data-rp-tab="${id}" class="${rocketpyTab === id ? "active" : ""}">${label}</button>`).join("")}
    </div>
    <div class="rp-body">${rocketpyTabHtml(rocketpyTab)}</div>
    <div class="rp-foot">
      <div class="rp-presets">
        <button type="button" data-act="rp-calisto" class="ghost compact">Calisto preset</button>
        <button type="button" data-act="rp-sounding" class="ghost compact">Sounding preset</button>
      </div>
      <div id="rp-py-status" class="rp-status ${rocketpyStatus.checked && !rocketpyStatus.available ? "bad" : ""}">${escapeHtml(status)}</div>
      <button type="button" data-act="rp-fly" class="primary">Fly trajectory</button>
    </div>`;
  overlay.querySelectorAll("[data-rp]").forEach((input) => {
    const apply = () => {
      const path = input.dataset.rp;
      if (input.tagName === "SELECT") {
        setRpPath(draft, path, input.value);
        return;
      }
      setRpPath(draft, path, input.value === "" ? "" : Number(input.value));
      if (path === "env.latitude" || path === "env.longitude" || path === "env.elevation_m") {
        refreshSiteMarkers();
        syncRocketPyLaunchFields();
      }
    };
    input.addEventListener("input", apply);
    input.addEventListener("change", apply);
  });
  overlay.querySelectorAll("[data-rp-tab]").forEach((btn) => {
    btn.addEventListener("click", () => {
      rocketpyTab = btn.dataset.rpTab;
      paintRocketPyOverlay();
    });
  });
  overlay.querySelector("[data-act='rp-close']")?.addEventListener("click", () => closeRocketPyBuilder());
  overlay.querySelector("[data-act='rp-pick-launch']")?.addEventListener("click", () => {
    startSitePick(rocketpyOpenId, "rp-launch");
  });
  overlay.querySelector("[data-act='rp-calisto']")?.addEventListener("click", () => {
    rocketpyDrafts.set(rocketpyOpenId, defaultRocketPySpec());
    paintRocketPyOverlay();
    render();
  });
  overlay.querySelector("[data-act='rp-sounding']")?.addEventListener("click", () => {
    rocketpyDrafts.set(rocketpyOpenId, soundingRocketPySpec());
    paintRocketPyOverlay();
    render();
  });
  overlay.querySelector("[data-act='rp-fly']")?.addEventListener("click", () => {
    runRocketPy(rocketpyOpenId);
  });
}

function syncRocketPyLaunchFields() {
  const overlay = els.rocketpyOverlay;
  if (!overlay || overlay.hidden || rocketpyOpenId == null) return;
  const draft = rocketpyDrafts.get(rocketpyOpenId);
  if (!draft) return;
  const lat = overlay.querySelector("[data-rp='env.latitude']");
  const lon = overlay.querySelector("[data-rp='env.longitude']");
  const elev = overlay.querySelector("[data-rp='env.elevation_m']");
  if (lat && document.activeElement !== lat) lat.value = draft.env.latitude;
  if (lon && document.activeElement !== lon) lon.value = draft.env.longitude;
  if (elev && draft.env.elevation_m != null && document.activeElement !== elev) {
    elev.value = draft.env.elevation_m;
  }
  const pick = overlay.querySelector("[data-act='rp-pick-launch']");
  if (pick) pick.textContent = `Pick launch on globe · ${fmtSite(draft.env.latitude, draft.env.longitude)}`;
}

async function refreshRocketPyStatus() {
  let el = document.getElementById("rp-py-status");
  if (el && !rocketpyStatus.available) {
    el.classList.remove("bad");
    el.textContent = "Loading 6DOF…";
  }
  try {
    const status = await invoke("rocketpy_status");
    rocketpyStatus = { checked: true, ...status };
  } catch (err) {
    rocketpyStatus = { checked: true, available: false, error: String(err) };
  }
  el = document.getElementById("rp-py-status");
  if (!el) return;
  if (rocketpyStatus.available) {
    el.classList.remove("bad");
    el.textContent = `6DOF ${rocketpyStatus.version || ""} · ${rocketpyStatus.python || "python"}`.trim();
  } else {
    el.classList.add("bad");
    el.textContent = rocketpyStatus.error || "6DOF is not ready";
  }
}

function openRocketPyBuilder(objectId) {
  const obj = riskModel.objects.find((o) => o.id === objectId);
  if (!obj) return;
  generateEngine.set(objectId, "rocketpy");
  objectMethod.set(objectId, "generate");
  rocketpyDraftFor(obj);
  rocketpyOpenId = objectId;
  rocketpyTab = "env";
  expandedObjectId = objectId;
  if (els.rocketpyOverlay) {
    els.rocketpyOverlay.hidden = false;
    paintRocketPyOverlay();
  }
  refreshRocketPyStatus();
  render();
  resizeGlobe();
}

function closeRocketPyBuilder(opts = {}) {
  const wasOpen = rocketpyOpenId != null || (els.rocketpyOverlay && !els.rocketpyOverlay.hidden);
  rocketpyOpenId = null;
  if (els.rocketpyOverlay) {
    els.rocketpyOverlay.hidden = true;
    els.rocketpyOverlay.innerHTML = "";
  }
  if (wasOpen) resizeGlobe();
  if (opts.render !== false) render();
}

function requireModeNameFor(objectId) {
  const card = document.querySelector(`[data-object-id="${objectId}"]`);
  const modeName = fieldText(card?.querySelector("[data-role='load-mode-name']"))
    || objectLoadName.get(objectId)
    || "";
  if (!modeName) {
    els.progress.textContent = "Enter a mode name, then fly 6DOF.";
    card?.querySelector("[data-role='load-mode-name']")?.focus();
    return "";
  }
  objectLoadName.set(objectId, modeName);
  return modeName;
}

function windSpecFromMission() {
  if (missionWind.type === "constant") {
    return {
      type: "constant",
      speed_mps: Number(missionWind.speed_mps),
      from_deg: Number(missionWind.from_deg),
    };
  }
  if (missionWind.type === "historical") {
    return {
      type: "historical",
      date: String(missionWind.date || defaultWindDate()),
      hour_utc: Number(missionWind.hour_utc) || 0,
    };
  }
  return { type: "off" };
}

function missionWindSummary() {
  if (missionWind.type === "constant") {
    return `Uses mission wind: ${missionWind.speed_mps} m/s from ${missionWind.from_deg}°.`;
  }
  if (missionWind.type === "historical") {
    const src = missionWind.source ? ` ${missionWind.source}.` : "";
    return `Uses mission wind: GFS ${missionWind.date || defaultWindDate()} ${String(missionWind.hour_utc).padStart(2, "0")}Z.${src}`;
  }
  return "Uses mission wind: off (set above).";
}

function applyMissionWind(wind) {
  const w = wind || { type: "off" };
  missionWind.type = w.type || "off";
  if (w.type === "constant") {
    missionWind.speed_mps = w.speed_mps ?? 15;
    missionWind.from_deg = w.from_deg ?? 270;
  }
  if (w.type === "historical") {
    missionWind.date = w.date || missionWind.date || defaultWindDate();
    missionWind.hour_utc = w.hour_utc ?? 12;
    missionWind.source = w.source || "";
  }
  if (!missionWind.date) missionWind.date = defaultWindDate();
  renderWindPanel();
}

function renderWindPanel() {
  const host = document.getElementById("wind-panel");
  if (!host) return;
  host.querySelectorAll("[data-wind]").forEach((btn) => {
    const on = btn.dataset.wind === missionWind.type;
    btn.className = on ? "compact" : "ghost compact";
  });
  if (!els.windFields) return;
  if (missionWind.type === "constant") {
    els.windFields.innerHTML = `<div class="gen-grid">
      <label>Speed m/s <input type="number" data-mw="speed_mps" min="0" max="150" step="0.5" value="${missionWind.speed_mps}" /></label>
      <label title="Meteorological direction the wind is coming from">From ° <input type="number" data-mw="from_deg" min="0" max="360" step="1" value="${missionWind.from_deg}" /></label>
      <div class="muted wind-hint">0 = north, 90 = east. Changing wind regenerates existing generated trajectories.</div>
    </div>`;
  } else if (missionWind.type === "historical") {
    const hours = Array.from({ length: 24 }, (_, h) => {
      const selected = Number(missionWind.hour_utc) === h ? " selected" : "";
      return `<option value="${h}"${selected}>${String(h).padStart(2, "0")}Z</option>`;
    }).join("");
    const src = missionWind.source
      ? `<div class="muted wind-hint">${escapeHtml(missionWind.source)} · changing date or hour regenerates generated trajectories</div>`
      : `<div class="muted wind-hint">GFS pressure-level winds (from 2021). Changing date or hour regenerates generated trajectories.</div>`;
    els.windFields.innerHTML = `<div class="gen-grid">
      <label>Date UTC <input type="date" data-mw="date" value="${escapeHtml(missionWind.date || defaultWindDate())}" /></label>
      <label>Hour <select data-mw="hour_utc">${hours}</select></label>
    </div>${src}`;
  } else {
    els.windFields.innerHTML = `<div class="muted wind-hint">No added wind. Atmosphere co-rotates with Earth. Switching away from this regenerates generated trajectories.</div>`;
  }
  els.windFields.querySelectorAll("[data-mw]").forEach((input) => {
    input.addEventListener("change", () => {
      const key = input.dataset.mw;
      missionWind[key] = input.type === "date" || key === "date" ? input.value : Number(input.value);
      persistMissionWind();
    });
  });
}

function bindWindPanel() {
  if (!missionWind.date) missionWind.date = defaultWindDate();
  document.querySelectorAll("#wind-panel [data-wind]").forEach((btn) => {
    btn.addEventListener("click", () => {
      missionWind.type = btn.dataset.wind;
      if (missionWind.type === "historical" && !missionWind.date) {
        missionWind.date = defaultWindDate();
      }
      renderWindPanel();
      persistMissionWind();
    });
  });
  renderWindPanel();
}

async function persistMissionWind() {
  setBusy(true);
  els.progress.textContent = missionWind.type === "historical"
    ? "Fetching winds and regenerating…"
    : "Updating wind…";
  try {
    const result = await invoke("set_mission_wind", ipcArgs({ wind: windSpecFromMission() }));
    riskModel = result.model || result;
    applyMissionWind(riskModel.wind);
    if (Array.isArray(result.tracks) && result.tracks.length) {
      for (const track of result.tracks) {
        const prev = tracks.get(track.id);
        tracks.set(track.id, prev ? { ...prev, ...track } : track);
      }
      addTracks(result.tracks);
    }
    applyRiskToTracks();
    await refreshRisk();
    await refreshOverlays({ immediate: true, force: true });
    await refreshMission();
    const n = Number(result.regenerated) || 0;
    els.progress.textContent = n
      ? `Recalculated ${n} computed traj for new wind`
      : "Wind updated — no computed trajectories to rebuild";
    if (result.elapsed_ms) els.statTime.textContent = `${result.elapsed_ms} ms`;
  } catch (err) {
    els.progress.textContent = String(err);
  } finally {
    setBusy(false);
    render();
  }
}

function fmtSite(lat, lon) {
  if (lat === "" || lon === "" || lat == null || lon == null || Number.isNaN(Number(lat))) {
    return "not set";
  }
  return `${Number(lat).toFixed(3)}°, ${Number(lon).toFixed(3)}°`;
}

function methodFor(obj) {
  if (objectMethod.has(obj.id)) return objectMethod.get(obj.id);
  if (obj.source === "generated" || obj.generate) return "generate";
  if (obj.source === "rocketpy" || obj.rocketpy) return "generate";
  if ((obj.modes || []).some((mode) => mode.track_count > 0)) return "files";
  return null;
}

function engineFor(obj) {
  if (generateEngine.has(obj.id)) return generateEngine.get(obj.id);
  if (obj.source === "rocketpy" || obj.rocketpy) return "rocketpy";
  return "3dof";
}

function setObjectMethod(objectId, method) {
  objectMethod.set(objectId, method);
  if (method !== "generate") {
    cancelGlobePick();
    pickKind = null;
    if (rocketpyOpenId === objectId) closeRocketPyBuilder({ render: false });
  }
  render();
}

function loadModeFor(obj) {
  const modes = obj.modes || [];
  const saved = objectLoadName.get(obj.id);
  if (saved) {
    const match = modes.find((mode) => mode.name.toLowerCase() === saved.toLowerCase());
    if (match) return match;
  }
  return modes.find((mode) => mode.name.toLowerCase() === "nominal") || modes[0] || null;
}

function requireModeName(host, obj) {
  const name = fieldText(host.querySelector("[data-role='load-mode-name']"));
  if (!name) {
    els.progress.textContent = "Enter a mode name, then add trajectories.";
    host.querySelector("[data-role='load-mode-name']")?.focus();
    return "";
  }
  objectLoadName.set(obj.id, name);
  return name;
}

function fillMethodChoice(host, obj) {
  if (!host) return;
  const method = methodFor(obj);
  const destMode = loadModeFor(obj);
  const modeName = objectLoadName.get(obj.id) || destMode?.name || suggestedModeName(obj);
  const destLabel = escapeHtml(modeName);
  const hasTracks = tracks.size > 0;
  const listId = `mode-names-${obj.id}`;
  const destOptions = (obj.modes || [])
    .map((mode) => `<option value="${escapeHtml(mode.name)}"></option>`)
    .join("");
  host.innerHTML = `
    <div class="muted method-label">Add trajectories to a mode</div>
    <label class="dest-row">Mode
      <input type="text" data-role="load-mode-name" list="${listId}" value="${escapeHtml(modeName)}" placeholder="e.g. Nominal" aria-label="Mode that receives new trajectories" />
    </label>
    <datalist id="${listId}">${destOptions}</datalist>
    <div class="method-toggle" role="group" aria-label="How to add trajectories">
      <button type="button" data-method="files" class="${method === "files" ? "compact" : "ghost compact"}">Load CSV</button>
      <button type="button" data-method="generate" class="${method === "generate" ? "compact" : "ghost compact"}">Generate</button>
      <button type="button" data-method="from-state" class="${method === "from-state" ? "compact" : "ghost compact"}" ${hasTracks ? "" : "disabled"} title="${hasTracks ? "Branch from a state on an existing trajectory" : "Need an existing trajectory first"}">From state</button>
    </div>
    <div class="method-body"></div>`;
  host.querySelector("[data-role='load-mode-name']")?.addEventListener("input", (ev) => {
    objectLoadName.set(obj.id, ev.target.value.trim());
  });
  host.querySelector("[data-method='files']").addEventListener("click", () => {
    const name = fieldText(host.querySelector("[data-role='load-mode-name']"));
    if (name) objectLoadName.set(obj.id, name);
    setObjectMethod(obj.id, "files");
  });
  host.querySelector("[data-method='generate']").addEventListener("click", () => {
    const name = fieldText(host.querySelector("[data-role='load-mode-name']"));
    if (name) objectLoadName.set(obj.id, name);
    setObjectMethod(obj.id, "generate");
  });
  host.querySelector("[data-method='from-state']").addEventListener("click", () => {
    if (!tracks.size) {
      els.progress.textContent = "Load or generate a trajectory first — From state needs initial conditions.";
      return;
    }
    const name = fieldText(host.querySelector("[data-role='load-mode-name']"));
    if (name) objectLoadName.set(obj.id, name);
    setObjectMethod(obj.id, "from-state");
  });
  const body = host.querySelector(".method-body");
  if (isObjectExpanded(obj.id) && method !== "from-state") {
    setStateMarker(null);
  }
  if (method === "files") {
    body.innerHTML = `
      <div class="dest-hint">Import recorded flights into <strong>${destLabel}</strong>. A new name creates another mode. Trajectories in a mode are exclusive.</div>
      <div class="gen-sites">
        <button type="button" data-act="load-files" class="primary compact">Choose files…</button>
        <button type="button" data-act="load-folder" class="ghost compact">Choose folder…</button>
      </div>
      <div class="muted wind-hint">CSV / text with time, lat, lon, alt. Folder import walks for matching files.</div>`;
    body.querySelector("[data-act='load-files']").addEventListener("click", () => {
      const name = requireModeName(host, obj);
      if (!name) return;
      runLoad("load_files", { object_id: obj.id, mode_name: name });
    });
    body.querySelector("[data-act='load-folder']").addEventListener("click", () => {
      const name = requireModeName(host, obj);
      if (!name) return;
      runLoad("load_folder", { object_id: obj.id, mode_name: name });
    });
    return;
  }
  if (method === "generate") {
    fillGenerateForm(body, obj, destLabel);
    return;
  }
  if (method === "from-state") {
    if (!hasTracks) {
      body.innerHTML = `<div class="dest-hint">From state needs an existing trajectory for initial conditions. Load CSVs or generate a flight first — usually on the vehicle object.</div>`;
      setStateMarker(null);
      return;
    }
    if (!isObjectExpanded(obj.id)) {
      body.innerHTML = `<div class="muted wind-hint">Expand this object to pick a source state.</div>`;
      return;
    }
    fillFromState(body, obj, destLabel);
    return;
  }
  body.innerHTML = `
    <div class="dest-hint">
      Trajectories go into <strong>${destLabel}</strong> on this object.
      <ol class="how-list">
        <li><strong>Load CSV</strong> — recorded flights from files or a folder.</li>
        <li><strong>Generate</strong> — 3DOF ballistic, or 6DOF in a builder beside the globe.</li>
        <li><strong>From state</strong> — spent stage, FTS, or nav-fail from a point on an existing traj.</li>
      </ol>
    </div>`;
}

function fillGenerateForm(body, obj, destLabel) {
  const engine = engineFor(obj);
  body.innerHTML = `
    <div class="method-toggle engine-toggle" role="group" aria-label="Trajectory engine">
      <button type="button" data-engine="3dof" class="${engine === "3dof" ? "compact" : "ghost compact"}">3DOF ballistic</button>
      <button type="button" data-engine="rocketpy" class="${engine === "rocketpy" ? "compact" : "ghost compact"}">6DOF</button>
    </div>
    <div data-role="gen-engine-body"></div>`;
  body.querySelectorAll("[data-engine]").forEach((btn) => {
    btn.addEventListener("click", () => {
      generateEngine.set(obj.id, btn.dataset.engine);
      if (btn.dataset.engine !== "rocketpy") closeRocketPyBuilder({ render: false });
      render();
    });
  });
  const inner = body.querySelector("[data-role='gen-engine-body']");
  if (engine === "rocketpy") fillRocketPyEntry(inner, obj, destLabel);
  else fillBallisticGenerate(inner, obj, destLabel);
}

function fillRocketPyEntry(body, obj, destLabel) {
  const draft = rocketpyDraftFor(obj);
  const open = rocketpyOpenId === obj.id;
  body.innerHTML = `
    <div class="dest-hint">6DOF flies a vehicle into <strong>${destLabel}</strong>. Inputs live in a builder beside the globe so this card stays small.</div>
    <div class="rp-card-summary">${escapeHtml(rocketpySummary(draft))}</div>
    <div class="rp-card-actions">
      <button type="button" data-act="open-rocketpy" class="${open ? "ghost compact" : "primary compact"}">${open ? "Builder is open →" : "Open 6DOF builder"}</button>
      <button type="button" data-act="run-rocketpy" class="${open ? "primary compact" : "ghost compact"}">Fly 6DOF</button>
    </div>
    <div class="muted wind-hint">Uses the app Python environment (rocketpy_backend/.venv). First open can take a few seconds.</div>`;
  body.querySelector("[data-act='open-rocketpy']").addEventListener("click", () => {
    openRocketPyBuilder(obj.id);
  });
  body.querySelector("[data-act='run-rocketpy']").addEventListener("click", () => {
    runRocketPy(obj.id);
  });
}

function fillBallisticGenerate(body, obj, destLabel) {
  const draft = draftFor(obj);
  const n = (v) => (v === "" || v == null ? "" : v);
  body.innerHTML = `
    <div class="dest-hint">Builds one 3DOF trajectory into <strong>${destLabel}</strong>. Min-energy burnout to the aimpoint, then ballistic coast. Existing tracks stay.</div>
    <div class="field-block">
      <div class="muted method-label">Launch site</div>
      <div class="gen-grid">
        <label>Latitude <input type="number" data-gen="launch_lat" min="-90" max="90" step="0.0001" value="${n(draft.launch_lat)}" /></label>
        <label>Longitude <input type="number" data-gen="launch_lon" min="-180" max="180" step="0.0001" value="${n(draft.launch_lon)}" /></label>
        <label>Elevation m <input type="number" data-gen="launch_alt_m" step="1" value="${n(draft.launch_alt_m)}" /></label>
      </div>
      <button type="button" data-act="pick-launch" class="ghost compact">Pick launch on globe · ${fmtSite(draft.launch_lat, draft.launch_lon)}</button>
    </div>
    <div class="field-block">
      <div class="muted method-label">Aimpoint</div>
      <div class="gen-grid">
        <label>Latitude <input type="number" data-gen="aim_lat" min="-90" max="90" step="0.0001" value="${n(draft.aim_lat)}" /></label>
        <label>Longitude <input type="number" data-gen="aim_lon" min="-180" max="180" step="0.0001" value="${n(draft.aim_lon)}" /></label>
        <label>Altitude m <input type="number" data-gen="aim_alt_m" step="1" value="${n(draft.aim_alt_m)}" /></label>
      </div>
      <button type="button" data-act="pick-aim" class="ghost compact">Pick aim on globe · ${fmtSite(draft.aim_lat, draft.aim_lon)}</button>
    </div>
    <div class="field-block">
      <div class="muted method-label">Vehicle</div>
      <div class="gen-grid">
        <label title="Mass / (Cd × reference area)">BC kg/m² <input type="number" data-gen="ballistic_coeff" min="5" max="50000" step="10" value="${draft.ballistic_coeff}" /></label>
        <label title="State after boost. Surface launch through dense air is not physical for long range.">Burnout m <input type="number" data-gen="burnout_alt_m" min="0" max="400000" step="1000" value="${draft.burnout_alt_m}" /></label>
      </div>
    </div>
    <button type="button" data-act="run-generate" class="primary compact">Generate trajectory</button>
    <div class="muted wind-hint">${missionWindSummary()} US-1976 atmosphere · J2 gravity · no lift.</div>`;
  body.querySelectorAll("[data-gen]").forEach((input) => {
    const apply = () => {
      const key = input.dataset.gen;
      draft[key] = input.value === "" ? "" : Number(input.value);
      if (/^(launch_|aim_)/.test(key)) {
        refreshSiteMarkers();
        syncGenerateSiteFields(obj.id);
      }
    };
    input.addEventListener("input", apply);
    input.addEventListener("change", apply);
  });
  body.querySelector("[data-act='pick-launch']").addEventListener("click", () => {
    startSitePick(obj.id, "launch");
  });
  body.querySelector("[data-act='pick-aim']").addEventListener("click", () => {
    startSitePick(obj.id, "aim");
  });
  body.querySelector("[data-act='run-generate']").addEventListener("click", () => {
    runGenerate(obj.id);
  });
}

function startSitePick(objectId, kind) {
  pickKind = { objectId, kind };
  els.progress.textContent = kind === "aim"
    ? "Click the globe for the aimpoint"
    : kind === "rp-launch"
      ? "Click the globe for 6DOF launch (right of the builder)"
      : "Click the globe for launch";
  beginGlobePick((lla) => {
    applySiteLla(objectId, kind === "rp-launch" ? "launch" : kind, lla, { rocketpy: kind === "rp-launch" });
    cancelGlobePick();
    pickKind = null;
    els.progress.textContent = kind === "aim"
      ? "Aimpoint set"
      : kind === "rp-launch"
        ? "6DOF launch set"
        : "Launch set — pick aim if needed";
  });
}

function siteAlt(value) {
  const n = Number(value);
  return Number.isFinite(n) ? n : 0;
}

function refreshSiteMarkers() {
  if (rocketpyOpenId != null) {
    const obj = riskModel.objects.find((o) => o.id === rocketpyOpenId);
    const draft = obj ? rocketpyDraftFor(obj) : rocketpyDrafts.get(rocketpyOpenId);
    const lat = Number(draft?.env?.latitude);
    const lon = Number(draft?.env?.longitude);
    setSiteMarkers(
      Number.isFinite(lat) && Number.isFinite(lon)
        ? { lat, lon, alt: siteAlt(draft.env.elevation_m) }
        : null,
      null,
    );
    return;
  }
  const expanded = riskModel.objects.find((o) => isObjectExpanded(o.id));
  const focus = expanded && methodFor(expanded) === "generate" ? expanded : null;
  if (!focus) {
    setSiteMarkers(null, null);
    return;
  }
  if (engineFor(focus) === "rocketpy") {
    const draft = rocketpyDraftFor(focus);
    const lat = Number(draft.env.latitude);
    const lon = Number(draft.env.longitude);
    setSiteMarkers(
      Number.isFinite(lat) && Number.isFinite(lon)
        ? { lat, lon, alt: siteAlt(draft.env.elevation_m) }
        : null,
      null,
    );
    return;
  }
  const draft = draftFor(focus);
  const launchOk = draft.launch_lat !== "" && Number.isFinite(Number(draft.launch_lat));
  const aimOk = draft.aim_lat !== "" && Number.isFinite(Number(draft.aim_lat));
  setSiteMarkers(
    launchOk
      ? { lat: Number(draft.launch_lat), lon: Number(draft.launch_lon), alt: siteAlt(draft.launch_alt_m) }
      : null,
    aimOk
      ? { lat: Number(draft.aim_lat), lon: Number(draft.aim_lon), alt: siteAlt(draft.aim_alt_m) }
      : null,
  );
}

function syncGenerateSiteFields(objectId) {
  const card = document.querySelector(`[data-object-id="${objectId}"]`);
  const draft = generateDrafts.get(objectId);
  if (!card || !draft) return;
  const set = (key, value) => {
    const el = card.querySelector(`[data-gen="${key}"]`);
    if (!el || document.activeElement === el) return;
    if (value === "" || value == null || Number.isNaN(Number(value))) return;
    el.value = String(value);
  };
  set("launch_lat", draft.launch_lat);
  set("launch_lon", draft.launch_lon);
  set("launch_alt_m", draft.launch_alt_m);
  set("aim_lat", draft.aim_lat);
  set("aim_lon", draft.aim_lon);
  set("aim_alt_m", draft.aim_alt_m);
  const launchBtn = card.querySelector("[data-act='pick-launch']");
  if (launchBtn) launchBtn.textContent = `Pick launch on globe · ${fmtSite(draft.launch_lat, draft.launch_lon)}`;
  const aimBtn = card.querySelector("[data-act='pick-aim']");
  if (aimBtn) aimBtn.textContent = `Pick aim on globe · ${fmtSite(draft.aim_lat, draft.aim_lon)}`;
}

function applySiteLla(objectId, kind, lla, opts = {}) {
  const useRocketPy = opts.rocketpy || rocketpyOpenId === objectId;
  if (useRocketPy) {
    const obj = riskModel.objects.find((o) => o.id === objectId) || { id: objectId };
    const draft = rocketpyDraftFor(obj);
    draft.env.latitude = lla.lat;
    draft.env.longitude = lla.lon;
    if (lla.alt != null && Number.isFinite(Number(lla.alt)) && opts.setAlt) {
      draft.env.elevation_m = Number(lla.alt);
    }
    syncRocketPyLaunchFields();
    refreshSiteMarkers();
    return;
  }
  const obj = riskModel.objects.find((o) => o.id === objectId);
  const draft = obj ? draftFor(obj) : generateDrafts.get(objectId);
  if (!draft) return;
  if (kind === "launch") {
    draft.launch_lat = lla.lat;
    draft.launch_lon = lla.lon;
    if (opts.setAlt && Number.isFinite(Number(lla.alt))) draft.launch_alt_m = Number(lla.alt);
    else if (draft.launch_alt_m === "" || draft.launch_alt_m == null) draft.launch_alt_m = 0;
  } else {
    draft.aim_lat = lla.lat;
    draft.aim_lon = lla.lon;
    if (opts.setAlt && Number.isFinite(Number(lla.alt))) draft.aim_alt_m = Number(lla.alt);
    else if (draft.aim_alt_m === "" || draft.aim_alt_m == null) draft.aim_alt_m = 0;
  }
  syncGenerateSiteFields(objectId);
  refreshSiteMarkers();
}

function onGlobeSiteChange({ kind, lat, lon, alt }) {
  if (rocketpyOpenId != null) {
    applySiteLla(rocketpyOpenId, "launch", { lat, lon, alt }, { rocketpy: true });
    return;
  }
  const expanded = riskModel.objects.find((o) => isObjectExpanded(o.id));
  if (!expanded || methodFor(expanded) !== "generate" || engineFor(expanded) === "rocketpy") {
    if (expanded && engineFor(expanded) === "rocketpy") {
      applySiteLla(expanded.id, "launch", { lat, lon, alt }, { rocketpy: true });
    }
    return;
  }
  applySiteLla(expanded.id, kind, { lat, lon, alt });
}

function syncCatalogs(model) {
  if (Array.isArray(model?.catalogs) && model.catalogs.length) {
    catalogs = model.catalogs;
    if (!catalogs.some((c) => c.id === simDraft.catalogId)) {
      simDraft.catalogId = catalogs[0].id;
    }
  }
}

function preferredSimTrackId() {
  const list = [...tracks.values()].filter(trackHasPath);
  if (!list.length) return 0;
  if (simDraft.trackId && list.some((t) => t.id === simDraft.trackId)) return simDraft.trackId;
  const named = list.find((t) => /nominal/i.test(t.name));
  return (named || list[0]).id;
}

function simTrackLabel(track) {
  const obj = riskModel.objects.find((o) => o.id === track.object_id);
  const mode = obj?.modes?.find((m) => m.id === track.failure_mode_id);
  return [obj?.name, mode?.name, track.name].filter(Boolean).join(" · ");
}

function currentCatalog() {
  return catalogs.find((c) => c.id === simDraft.catalogId) || catalogs[0] || null;
}

function fmtSimTime(sample) {
  if (!sample) return "—";
  const t = Number(sample.time_s);
  const label = sample.has_clock ? `t = ${t.toFixed(1)} s` : `sample ${t.toFixed(0)}`;
  const altKm = (Number(sample.alt_m) / 1000).toFixed(1);
  const spd = Number(sample.speed_mps).toFixed(0);
  const hdg = ((Number(sample.heading_deg) % 360) + 360) % 360;
  return `${label} · ${altKm} km · ${spd} m/s · hdg ${hdg.toFixed(0)}°`;
}

function fmtStageReadout(sample) {
  const dist = simDraft.stage.dist || "point";
  if (dist === "point") return fmtSimTime(sample);
  const n = Math.max(1, Number(simDraft.stage.count) || 1);
  if (dist === "uniform") {
    const a = Number(simDraft.stage.tMin);
    const b = Number(simDraft.stage.tMax);
    return `Uniform ${fmtTimeNum(a)}–${fmtTimeNum(b)} s · ${n} samples${sample ? ` · center ${fmtSimTime(sample)}` : ""}`;
  }
  const mean = simDraft.timeS ?? sample?.time_s;
  const w = normalStageWindow(mean, sample?.time_start, sample?.time_end);
  return `Normal μ=${fmtTimeNum(mean)} s, σ=${fmtTimeNum(simDraft.stage.sigma)} s · ±${fmtTimeNum(w.n)}σ (${fmtTimeNum(w.tMin)}–${fmtTimeNum(w.tMax)} s) · ${n} samples`;
}

function stageNSigma() {
  const n = Number(simDraft.stage.nSigma);
  if (!Number.isFinite(n) || n <= 0) return 3;
  return Math.min(10, Math.max(0.1, n));
}

function normalStageWindow(mean, t0, t1) {
  const m = Number(mean);
  const sigma = Math.max(Number(simDraft.stage.sigma) || 0, 1e-6);
  const n = stageNSigma();
  const lo = Number.isFinite(Number(t0)) ? Number(t0) : -Infinity;
  const hi = Number.isFinite(Number(t1)) ? Number(t1) : Infinity;
  return {
    tMin: Math.min(Math.max(m - n * sigma, lo), hi),
    tMax: Math.min(Math.max(m + n * sigma, lo), hi),
    n,
  };
}

function simHost() {
  return document.querySelector(".object-card:not(.collapsed) .method-body");
}

function ensureStageWindow(t0, t1, t) {
  const span = Math.max(Number(t1) - Number(t0), 0);
  const dt = Math.max(1, Math.min(span * 0.05, 10));
  const lo = Number(t0);
  const hi = Number(t1);
  const mid = Number(t);
  if (simDraft.stage.tMin == null || !Number.isFinite(Number(simDraft.stage.tMin))) {
    simDraft.stage.tMin = Math.max(lo, mid - dt);
  }
  if (simDraft.stage.tMax == null || !Number.isFinite(Number(simDraft.stage.tMax))) {
    simDraft.stage.tMax = Math.min(hi, mid + dt);
  }
  simDraft.stage.tMin = Math.min(Math.max(Number(simDraft.stage.tMin), lo), hi);
  simDraft.stage.tMax = Math.min(Math.max(Number(simDraft.stage.tMax), lo), hi);
  if (simDraft.stage.tMax < simDraft.stage.tMin) {
    const x = simDraft.stage.tMin;
    simDraft.stage.tMin = simDraft.stage.tMax;
    simDraft.stage.tMax = x;
  }
}

function fmtTimeNum(t) {
  const n = Number(t);
  if (!Number.isFinite(n)) return "";
  return String(Number(n.toFixed(3)));
}

function stageTimeBlock(t0, t1, t) {
  const dist = simDraft.stage.dist || "point";
  const step = Math.max((t1 - t0) / 400, 0.05);
  if (dist === "uniform") {
    ensureStageWindow(t0, t1, t);
    return `
      <div class="sim-time dual">
        <input type="range" data-sim="time-lo" min="${t0}" max="${t1}" step="${step}" value="${simDraft.stage.tMin}" />
        <input type="range" data-sim="time-hi" min="${t0}" max="${t1}" step="${step}" value="${simDraft.stage.tMax}" />
      </div>`;
  }
  return `
    <div class="sim-time">
      <input type="range" data-sim="time" min="${t0}" max="${t1}" step="${step}" value="${t}" />
    </div>`;
}

function fillFromState(host, obj, destLabel) {
  simDraft.destObjectId = obj.id;
  const list = [...tracks.values()].filter(trackHasPath);
  if (!list.length) {
    host.innerHTML = `<div class="dest-hint">From state needs an existing trajectory. Load CSVs or generate a flight first.</div>`;
    setStateMarker(null);
    return;
  }
  simDraft.trackId = preferredSimTrackId();
  const trackOpts = list
    .map((t) => {
      const sel = t.id === simDraft.trackId ? " selected" : "";
      return `<option value="${t.id}"${sel}>${escapeHtml(simTrackLabel(t))}</option>`;
    })
    .join("");
  const sample = simDraft.sample;
  const t0 = sample?.time_start ?? 0;
  const t1 = sample?.time_end ?? 1;
  const t = sample ? sample.time_s : (simDraft.timeS ?? t0);
  const kind = simDraft.kind;
  const cat = currentCatalog();
  const catOpts = catalogs
    .map((c) => {
      const sel = c.id === simDraft.catalogId ? " selected" : "";
      return `<option value="${c.id}"${sel}>${escapeHtml(c.name)} (${c.pieces.reduce((n, p) => n + (p.count || 1), 0)} pcs)</option>`;
    })
    .join("");
  const dist = simDraft.stage.dist || "point";
  if (kind === "stage" && dist === "uniform") ensureStageWindow(t0, t1, t);
  const timeBlock = kind === "stage" ? stageTimeBlock(t0, t1, t) : `
    <div class="sim-time">
      <input type="range" data-sim="time" min="${t0}" max="${t1}" step="${Math.max((t1 - t0) / 400, 0.05)}" value="${t}" />
    </div>`;
  host.innerHTML = `
    <div class="dest-hint">Sample a state on an existing trajectory, then coast from there into <strong>${destLabel}</strong> on this object. Put a spent stage or debris cloud on its own object if it should count in addition to the vehicle.</div>
    <label class="dest-row">Source traj
      <select data-sim="track">${trackOpts}</select>
    </label>
    ${timeBlock}
    <div class="sim-readout" id="sim-readout">${escapeHtml(kind === "stage" ? fmtStageReadout(sample) : fmtSimTime(sample))}</div>
    <div class="method-toggle sim-kinds" role="group" aria-label="Branch kind">
      <button type="button" data-kind="stage" class="${kind === "stage" ? "compact" : "ghost compact"}">Spent stage</button>
      <button type="button" data-kind="fts" class="${kind === "fts" ? "compact" : "ghost compact"}">FTS</button>
      <button type="button" data-kind="nav" class="${kind === "nav" ? "compact" : "ghost compact"}">Nav + FTS</button>
    </div>
    <div id="sim-kind-body"></div>`;
  const body = host.querySelector("#sim-kind-body");
  if (kind === "stage") {
    const d = dist;
    body.innerHTML = `
      <div class="muted wind-hint">Ballistic coast from this state using a constant ballistic coefficient, stopping at the parent landing elevation. Use a lower BC than the parent if the stage should fall short. Typical for a dropped booster on a separate inclusive object.</div>
      <div class="muted method-label">Separation time</div>
      <div class="method-toggle sim-kinds" role="group" aria-label="Separation time distribution">
        <button type="button" data-dist="point" class="${d === "point" ? "compact" : "ghost compact"}">Point</button>
        <button type="button" data-dist="uniform" class="${d === "uniform" ? "compact" : "ghost compact"}">Uniform</button>
        <button type="button" data-dist="normal" class="${d === "normal" ? "compact" : "ghost compact"}">Normal</button>
      </div>
      <div class="gen-grid">
        ${d === "point" ? `<label>t s <input type="number" data-sim="time-num" step="0.1" value="${fmtTimeNum(t)}" /></label>` : ""}
        ${d === "uniform" ? `
          <label>t min s <input type="number" data-sim="time-min-num" step="0.1" value="${fmtTimeNum(simDraft.stage.tMin)}" /></label>
          <label>t max s <input type="number" data-sim="time-max-num" step="0.1" value="${fmtTimeNum(simDraft.stage.tMax)}" /></label>` : ""}
        ${d === "normal" ? `
          <label>μ s <input type="number" data-sim="time-num" step="0.1" value="${fmtTimeNum(t)}" /></label>
          <label>σ s <input type="number" data-sim="stage-sigma" min="0.01" step="0.1" value="${fmtTimeNum(simDraft.stage.sigma)}" /></label>
          <label title="Keep samples inside μ ± this many standard deviations, then the trajectory span.">max Nσ <input type="number" data-sim="stage-nsigma" min="0.1" max="10" step="0.1" value="${fmtTimeNum(stageNSigma())}" /></label>` : ""}
        <label>BC kg/m² <input type="number" data-sim="stage-bc" min="5" max="50000" step="10" value="${simDraft.stage.ballistic_coeff}" /></label>
        ${d !== "point" ? `
          <label>Samples <input type="number" data-sim="stage-count" min="1" max="200" step="1" value="${simDraft.stage.count}" /></label>
          <label>Seed <input type="number" data-sim="stage-seed" min="1" step="1" value="${simDraft.stage.seed}" /></label>` : ""}
      </div>
      <button type="button" data-act="run-stage" class="primary compact">Propagate stage</button>`;
  } else if (kind === "fts") {
    body.innerHTML = `
      <div class="muted wind-hint">Breaks up at this state and coasts every catalogue piece to the ground. Fragments share this mode as an exclusive ensemble.</div>
      <div class="gen-grid">
        <label>Seed <input type="number" data-sim="seed" min="1" step="1" value="${simDraft.seed}" /></label>
      </div>
      <label class="dest-row">Catalogue
        <select data-sim="catalog">${catOpts}</select>
      </label>
      <button type="button" data-act="toggle-cat" class="ghost compact">${simDraft.showCatalog ? "Hide catalogue" : "Edit catalogue"}</button>
      <div id="sim-catalog"></div>
      <button type="button" data-act="run-fts" class="primary compact">Propagate FTS debris</button>`;
  } else {
    const chips = simDraft.nav.times
      .map((t, i) => `<span class="sim-chip">${Number(t).toFixed(1)} s <button type="button" data-rm-time="${i}" aria-label="Remove time">×</button></span>`)
      .join("");
    body.innerHTML = `
      <div class="muted wind-hint">At each time the vehicle does a max-g horizontal turn, then FTS fires. Fragments keep that inertial velocity, so on a ballistic coast they still land near the original impact unless the turn is long or BC is low. Default 5 s, speed held. Add times or use the slider time.</div>
      <div class="gen-grid">
        <label>Max g <input type="number" data-sim="nav-g" min="0.1" max="20" step="0.5" value="${simDraft.nav.maxG}" /></label>
        <label>Turn s <input type="number" data-sim="nav-dur" min="0.1" max="60" step="0.5" value="${simDraft.nav.durationS}" /></label>
        <label>Sides
          <select data-sim="nav-side">
            <option value="both"${simDraft.nav.side === "both" ? " selected" : ""}>Both</option>
            <option value="left"${simDraft.nav.side === "left" ? " selected" : ""}>Left</option>
            <option value="right"${simDraft.nav.side === "right" ? " selected" : ""}>Right</option>
          </select>
        </label>
        <label>Seed <input type="number" data-sim="seed" min="1" step="1" value="${simDraft.seed}" /></label>
      </div>
      <div class="gen-grid">
        <label>Every s <input type="number" data-sim="nav-every" min="0.5" max="120" step="0.5" value="${simDraft.nav.everyS}" /></label>
        <div style="display:flex;align-items:end;gap:6px">
          <button type="button" data-act="add-time" class="ghost compact">Add current time</button>
          <button type="button" data-act="fill-times" class="ghost compact">Fill span</button>
        </div>
      </div>
      <div class="sim-chips">${chips || `<span class="muted">No times yet — Run uses the slider time, or add/fill times.</span>`}</div>
      <label class="dest-row">Catalogue
        <select data-sim="catalog">${catOpts}</select>
      </label>
      <button type="button" data-act="toggle-cat" class="ghost compact">${simDraft.showCatalog ? "Hide catalogue" : "Edit catalogue"}</button>
      <div id="sim-catalog"></div>
      <button type="button" data-act="run-nav" class="primary compact">Run nav + FTS</button>`;
  }
  if (simDraft.showCatalog && (kind === "fts" || kind === "nav")) {
    renderCatalogEditor(host.querySelector("#sim-catalog"), cat);
  }
  host.querySelector("[data-sim='track']")?.addEventListener("change", (ev) => {
    simDraft.trackId = Number(ev.target.value);
    simDraft.timeS = null;
    simDraft.sample = null;
    refreshSimSample(true);
  });
  host.querySelector("[data-sim='time']")?.addEventListener("input", (ev) => {
    simDraft.timeS = Number(ev.target.value);
    const num = host.querySelector("[data-sim='time-num']");
    if (num) num.value = fmtTimeNum(simDraft.timeS);
    scheduleSimSample();
  });
  const bindRange = (sel, key) => {
    host.querySelector(sel)?.addEventListener("input", (ev) => {
      let v = Number(ev.target.value);
      if (key === "tMin" && v > Number(simDraft.stage.tMax)) v = Number(simDraft.stage.tMax);
      if (key === "tMax" && v < Number(simDraft.stage.tMin)) v = Number(simDraft.stage.tMin);
      simDraft.stage[key] = v;
      ev.target.value = String(v);
      const numSel = key === "tMin" ? "[data-sim='time-min-num']" : "[data-sim='time-max-num']";
      const num = host.querySelector(numSel);
      if (num) num.value = fmtTimeNum(v);
      const readout = host.querySelector("#sim-readout");
      if (readout) readout.textContent = fmtStageReadout(simDraft.sample);
    });
  };
  bindRange("[data-sim='time-lo']", "tMin");
  bindRange("[data-sim='time-hi']", "tMax");
  host.querySelectorAll("[data-kind]").forEach((btn) => {
    btn.addEventListener("click", () => {
      simDraft.kind = btn.dataset.kind;
      render();
    });
  });
  body.querySelectorAll("[data-dist]").forEach((btn) => {
    btn.addEventListener("click", () => {
      simDraft.stage.dist = btn.dataset.dist;
      if (simDraft.stage.dist !== "point" && Number(simDraft.stage.count) < 2) {
        simDraft.stage.count = 20;
      }
      render();
    });
  });
  const bindVal = (sel, onVal) => {
    const el = body.querySelector(sel);
    if (!el) return;
    const apply = (ev) => onVal(ev.target.value);
    el.addEventListener("input", apply);
    el.addEventListener("change", apply);
  };
  bindVal("[data-sim='stage-bc']", (v) => { simDraft.stage.ballistic_coeff = Number(v); });
  bindVal("[data-sim='stage-count']", (v) => { simDraft.stage.count = Math.max(1, Math.min(200, Number(v) || 1)); });
  bindVal("[data-sim='stage-seed']", (v) => { simDraft.stage.seed = Number(v) || 1; });
  bindVal("[data-sim='stage-sigma']", (v) => {
    simDraft.stage.sigma = Number(v);
    const readout = host.querySelector("#sim-readout");
    if (readout) readout.textContent = fmtStageReadout(simDraft.sample);
  });
  bindVal("[data-sim='stage-nsigma']", (v) => {
    const n = Number(v);
    if (!Number.isFinite(n) || n <= 0) return;
    simDraft.stage.nSigma = Math.min(10, Math.max(0.1, n));
    const readout = host.querySelector("#sim-readout");
    if (readout) readout.textContent = fmtStageReadout(simDraft.sample);
  });
  bindVal("[data-sim='time-num']", (v) => {
    const n = Number(v);
    if (!Number.isFinite(n)) return;
    simDraft.timeS = n;
    const slider = host.querySelector("[data-sim='time']");
    if (slider) slider.value = String(n);
    scheduleSimSample();
  });
  bindVal("[data-sim='time-min-num']", (v) => {
    const n = Number(v);
    if (!Number.isFinite(n)) return;
    simDraft.stage.tMin = n;
    const slider = host.querySelector("[data-sim='time-lo']");
    if (slider) slider.value = String(n);
    const readout = host.querySelector("#sim-readout");
    if (readout) readout.textContent = fmtStageReadout(simDraft.sample);
  });
  bindVal("[data-sim='time-max-num']", (v) => {
    const n = Number(v);
    if (!Number.isFinite(n)) return;
    simDraft.stage.tMax = n;
    const slider = host.querySelector("[data-sim='time-hi']");
    if (slider) slider.value = String(n);
    const readout = host.querySelector("#sim-readout");
    if (readout) readout.textContent = fmtStageReadout(simDraft.sample);
  });
  bindVal("[data-sim='nav-g']", (v) => { simDraft.nav.maxG = Number(v); });
  bindVal("[data-sim='nav-dur']", (v) => { simDraft.nav.durationS = Number(v); });
  bindVal("[data-sim='nav-side']", (v) => { simDraft.nav.side = v; });
  bindVal("[data-sim='nav-every']", (v) => { simDraft.nav.everyS = Number(v); });
  bindVal("[data-sim='seed']", (v) => { simDraft.seed = Number(v) || 1; });
  body.querySelector("[data-sim='catalog']")?.addEventListener("change", (ev) => {
    simDraft.catalogId = Number(ev.target.value);
    render();
  });
  if (!simDraft.sample || simDraft.sample.track_id !== simDraft.trackId) {
    refreshSimSample(false);
  } else {
    setStateMarker(simDraft.sample);
  }
}

function renderCatalogEditor(host, catalog) {
  if (!host || !catalog) return;
  const rows = catalog.pieces
    .map((p, i) => `
      <tr>
        <td class="cat-name"><input data-cat="${i}" data-field="name" type="text" value="${escapeHtml(p.name)}" /></td>
        <td><input data-cat="${i}" data-field="ballistic_coeff" type="number" min="5" max="50000" step="5" value="${p.ballistic_coeff}" /></td>
        <td><input data-cat="${i}" data-field="delta_v_mps" type="number" min="0" max="2000" step="5" value="${p.delta_v_mps}" /></td>
        <td><input data-cat="${i}" data-field="count" type="number" min="1" max="200" step="1" value="${p.count || 1}" /></td>
        <td><button type="button" class="ghost compact" data-del-piece="${i}">✕</button></td>
      </tr>`)
    .join("");
  host.innerHTML = `
    <label class="dest-row">Catalogue name <input type="text" data-cat-name value="${escapeHtml(catalog.name)}" /></label>
    <table class="catalog-table">
      <thead><tr><th>Piece</th><th>BC</th><th>Δv m/s</th><th>n</th><th></th></tr></thead>
      <tbody>${rows}</tbody>
    </table>
    <div class="sim-chips">
      <button type="button" data-act="add-piece" class="ghost compact">+ Piece</button>
    </div>
    <div class="muted wind-hint">BC is kg/m². Δv is the breakup impulse; direction is sampled uniformly on the sphere.</div>`;
  host.querySelector("[data-cat-name]")?.addEventListener("change", (ev) => {
    catalog.name = ev.target.value;
    persistCatalog(catalog);
  });
  host.querySelectorAll("[data-cat]").forEach((input) => {
    input.addEventListener("change", () => {
      const piece = catalog.pieces[Number(input.dataset.cat)];
      if (!piece) return;
      const field = input.dataset.field;
      piece[field] = field === "name" ? input.value : Number(input.value);
      persistCatalog(catalog);
    });
  });
  host.querySelectorAll("[data-del-piece]").forEach((btn) => {
    btn.addEventListener("click", () => {
      catalog.pieces.splice(Number(btn.dataset.delPiece), 1);
      persistCatalog(catalog);
    });
  });
  host.querySelector("[data-act='add-piece']")?.addEventListener("click", () => {
    catalog.pieces.push({ name: "Piece", ballistic_coeff: 80, delta_v_mps: 80, count: 1 });
    persistCatalog(catalog);
  });
}

async function persistCatalog(catalog) {
  try {
    riskModel = await invoke("upsert_debris_catalog", ipcArgs({ catalog }));
    syncCatalogs(riskModel);
    render();
    await refreshMission();
  } catch (err) {
    els.progress.textContent = String(err);
  }
}

async function fillNavTimes() {
  readSimForm();
  if (!simDraft.trackId) simDraft.trackId = preferredSimTrackId();
  const track = tracks.get(simDraft.trackId);
  const s = simDraft.sample || (track
    ? { time_start: track.time_start ?? 0, time_end: track.time_end ?? 1 }
    : null);
  if (!s || !simDraft.trackId) {
    if (els.progress) els.progress.textContent = "Pick a source trajectory first.";
    return;
  }
  const every = Number(simDraft.nav.everyS) || 10;
  const cat = currentCatalog();
  const pieces = cat?.pieces?.reduce((n, p) => n + (Number(p.count) || 1), 0) || 22;
  const sides = simDraft.nav.side === "both" ? 2 : 1;
  const maxTimes = Math.max(1, Math.floor(1800 / Math.max(1, pieces * sides)));
  const span = Number(s.time_end) - Number(s.time_start);
  const lo = Number(s.time_start) + Math.max(0, span) * 0.12;
  const hi = Number(s.time_end) - Math.max(0, span) * 0.12;
  const times = [];
  for (let t = lo; t <= hi + 1e-9; t += every) {
    times.push(Number(t.toFixed(3)));
    if (times.length >= maxTimes) break;
  }
  if (!times.length) times.push(Number(((lo + hi) / 2).toFixed(3)));
  const airborne = await pickInFlightTimes(times);
  simDraft.nav.times = airborne.length ? airborne : times;
  if (els.progress) {
    els.progress.textContent = airborne.length
      ? `Filled ${airborne.length} in-flight time(s).`
      : "Fill span found no in-flight times — move the slider off the pad or landing.";
  }
}

function scheduleSimSample() {
  if (sampleTimer) clearTimeout(sampleTimer);
  sampleTimer = setTimeout(() => {
    sampleTimer = null;
    refreshSimSample(false);
  }, 80);
}

async function refreshSimSample(rerender) {
  if (!simDraft.trackId || !tracks.has(simDraft.trackId)) return;
  const track = tracks.get(simDraft.trackId);
  let time = simDraft.timeS;
  if (simDraft.kind === "stage" && simDraft.stage.dist === "uniform"
      && Number.isFinite(Number(simDraft.stage.tMin))
      && Number.isFinite(Number(simDraft.stage.tMax))) {
    time = (Number(simDraft.stage.tMin) + Number(simDraft.stage.tMax)) / 2;
  } else if (time == null) {
    const a = track.time_start ?? 0;
    const b = track.time_end ?? 1;
    time = (a + b) * 0.5;
  }
  try {
    simDraft.sample = await invoke("sample_state_vector", ipcArgs({ trackId: simDraft.trackId, timeS: time }));
    simDraft.timeS = simDraft.sample.time_s;
    setStateMarker(simDraft.sample);
    const readout = document.getElementById("sim-readout");
    if (readout) {
      readout.textContent = simDraft.kind === "stage" ? fmtStageReadout(simDraft.sample) : fmtSimTime(simDraft.sample);
    }
    const slider = simHost()?.querySelector("[data-sim='time']");
    if (slider && simDraft.sample) {
      slider.min = String(simDraft.sample.time_start);
      slider.max = String(simDraft.sample.time_end);
      slider.value = String(simDraft.sample.time_s);
    }
    if (rerender) render();
  } catch (err) {
    els.progress.textContent = String(err);
  }
}

function readSimForm() {
  const host = simHost();
  if (!host) return;
  const val = (sel) => host.querySelector(sel)?.value;
  const num = (sel) => {
    const n = Number(val(sel));
    return Number.isFinite(n) ? n : null;
  };
  const track = num("[data-sim='track']");
  if (track) simDraft.trackId = track;
  const time = num("[data-sim='time']") ?? num("[data-sim='time-num']");
  if (time != null) simDraft.timeS = time;
  const tMin = num("[data-sim='time-min-num']") ?? num("[data-sim='time-lo']");
  if (tMin != null) simDraft.stage.tMin = tMin;
  const tMax = num("[data-sim='time-max-num']") ?? num("[data-sim='time-hi']");
  if (tMax != null) simDraft.stage.tMax = tMax;
  const stageBc = num("[data-sim='stage-bc']");
  if (stageBc != null) simDraft.stage.ballistic_coeff = stageBc;
  const stageCount = num("[data-sim='stage-count']");
  if (stageCount != null) simDraft.stage.count = stageCount;
  const stageSeed = num("[data-sim='stage-seed']");
  if (stageSeed != null) simDraft.stage.seed = stageSeed;
  const sigma = num("[data-sim='stage-sigma']");
  if (sigma != null) simDraft.stage.sigma = sigma;
  const nSigma = num("[data-sim='stage-nsigma']");
  if (nSigma != null) simDraft.stage.nSigma = nSigma;
  const maxG = num("[data-sim='nav-g']");
  if (maxG != null) simDraft.nav.maxG = maxG;
  const dur = num("[data-sim='nav-dur']");
  if (dur != null) simDraft.nav.durationS = dur;
  const side = val("[data-sim='nav-side']");
  if (side) simDraft.nav.side = side;
  const every = num("[data-sim='nav-every']");
  if (every != null) simDraft.nav.everyS = every;
  const seed = num("[data-sim='seed']");
  if (seed != null) simDraft.seed = seed;
  const catalog = num("[data-sim='catalog']");
  if (catalog != null && catalog > 0) simDraft.catalogId = catalog;
}

function destModeName(objectId) {
  const card = els.objectList?.querySelector(`[data-object-id="${objectId}"]`);
  return fieldText(card?.querySelector("[data-role='load-mode-name']"))
    || objectLoadName.get(objectId)
    || "";
}

function simDestination() {
  const expandedId = Number(document.querySelector(".object-card:not(.collapsed)")?.dataset.objectId);
  const objectId = simDraft.destObjectId
    || (Number.isFinite(expandedId) ? expandedId : null)
    || null;
  const modeName = destModeName(objectId);
  return { objectId, objectName: null, modeName };
}

function ensureCatalogId() {
  if (simDraft.catalogId && catalogs.some((c) => c.id === simDraft.catalogId)) {
    return simDraft.catalogId;
  }
  if (catalogs[0]?.id) {
    simDraft.catalogId = catalogs[0].id;
    return simDraft.catalogId;
  }
  return simDraft.catalogId || 1;
}

async function ensureInFlightSample(minAlt = 80) {
  const trackId = simDraft.trackId || preferredSimTrackId();
  if (!trackId) return null;
  simDraft.trackId = trackId;
  if (!simDraft.sample || simDraft.sample.track_id !== simDraft.trackId) {
    await refreshSimSample(false);
  }
  const currentAlt = Number(simDraft.sample?.alt_m);
  const highEnough = Number.isFinite(currentAlt) && currentAlt >= Math.max(minAlt, 500);
  if (highEnough) return simDraft.sample;
  const track = tracks.get(simDraft.trackId);
  const a = Number(track?.time_start ?? simDraft.sample?.time_start ?? 0);
  const b = Number(track?.time_end ?? simDraft.sample?.time_end ?? 1);
  const probes = [0.25, 0.4, 0.55, 0.7].map((f) => a + f * (b - a));
  let best = simDraft.sample;
  for (const t of probes) {
    try {
      const s = await invoke("sample_state_vector", ipcArgs({ trackId: simDraft.trackId, timeS: t }));
      if (!best || Number(s.alt_m) > Number(best.alt_m || -1e9)) best = s;
    } catch {
      /* keep probing */
    }
  }
  if (best) {
    simDraft.sample = best;
    simDraft.timeS = best.time_s;
    setStateMarker(best);
  }
  return best;
}

async function pickInFlightTimes(times, minAlt = 80) {
  const trackId = simDraft.trackId || preferredSimTrackId();
  if (!trackId) return [];
  const seen = new Set();
  const airborne = [];
  for (const raw of times) {
    const t = Number(raw);
    if (!Number.isFinite(t) || seen.has(t)) continue;
    seen.add(t);
    try {
      const s = await invoke("sample_state_vector", ipcArgs({ trackId, timeS: t }));
      if (Number(s.alt_m) >= minAlt) airborne.push(Number(s.time_s));
    } catch {
      /* skip times that cannot be sampled */
    }
    if (airborne.length >= 40) break;
  }
  return airborne;
}

async function ingestSimResult(result, label) {
  if (simDraft.destObjectId) expandedObjectId = simDraft.destObjectId;
  pendingExpandLast = false;
  lastPointKey = "";
  lastWeightKey = "";
  for (const track of result.tracks || []) {
    tracks.set(track.id, track);
  }
  addTracks(result.tracks || []);
  await refreshRisk();
  const path = (result.tracks || []).filter(trackHasPath);
  const debris = (result.tracks || []).filter((t) => (t.weight ?? 1) > 0);
  const toFit = debris.length ? debris : path.length ? path : result.tracks;
  if (toFit?.length) {
    fitAll(toFit.map((t) => t.bounds).filter(Boolean));
    selectTrack(path[0]?.id || debris[0]?.id || result.tracks[0].id);
  }
  await refreshOverlays({ immediate: true, force: true });
  await refreshMission();
  els.statTime.textContent = result.elapsed_ms ? `${result.elapsed_ms} ms` : "—";
  const n = (result.tracks || []).length;
  els.progress.textContent = n ? label : `${label} — no tracks returned`;
}

async function runSimStage() {
  readSimForm();
  if (!simDraft.trackId) simDraft.trackId = preferredSimTrackId();
  if (!simDraft.trackId) {
    els.progress.textContent = "Pick a source trajectory first.";
    return;
  }
  const dest = simDestination();
  if (!dest.objectId || !dest.modeName) {
    els.progress.textContent = "Enter a mode name, then propagate the spent stage onto this object.";
    return;
  }
  const dist = simDraft.stage.dist || "point";
  if (dist === "uniform" && simDraft.stage.tMin != null && simDraft.stage.tMax != null) {
    simDraft.timeS = (Number(simDraft.stage.tMin) + Number(simDraft.stage.tMax)) / 2;
  }
  if (!simDraft.sample || simDraft.sample.track_id !== simDraft.trackId) {
    await refreshSimSample(false);
  }
  const sliderTime = Number(simHost()?.querySelector("[data-sim='time']")?.value);
  const timeS = simDraft.sample?.time_s ?? simDraft.timeS ?? (Number.isFinite(sliderTime) ? sliderTime : null);
  if (timeS == null || Number.isNaN(Number(timeS))) {
    els.progress.textContent = "Pick a separation time on the source trajectory.";
    return;
  }
  setBusy(true);
  els.progress.textContent = dist === "point" ? "Propagating spent stage…" : `Propagating spent stage (${simDraft.stage.count} times)…`;
  try {
    const result = await invoke("simulate_spent_stage", ipcArgs({
      spec: {
        source_track_id: simDraft.trackId,
        time_s: Number(timeS),
        ballistic_coeff: Number(simDraft.stage.ballistic_coeff),
        object_id: dest.objectId,
        object_name: null,
        mode_name: dest.modeName,
        dist,
        count: dist === "point" ? 1 : Math.max(1, Number(simDraft.stage.count) || 1),
        t_min: dist === "uniform" ? Number(simDraft.stage.tMin) : null,
        t_max: dist === "uniform" ? Number(simDraft.stage.tMax) : null,
        sigma_s: dist === "normal" ? Number(simDraft.stage.sigma) : null,
        n_sigma: dist === "normal" ? stageNSigma() : 3,
        seed: Number(simDraft.stage.seed) || 1,
      },
    }));
    await ingestSimResult(result, `Spent stage · ${result.tracks.length} traj`);
  } catch (err) {
    els.progress.textContent = String(err);
  } finally {
    setBusy(false);
    render();
  }
}

async function runSimFts() {
  readSimForm();
  if (!simDraft.trackId) simDraft.trackId = preferredSimTrackId();
  if (!simDraft.trackId) {
    els.progress.textContent = "Pick a source trajectory first.";
    return;
  }
  const dest = simDestination();
  if (!dest.objectId || !dest.modeName) {
    els.progress.textContent = "Enter a mode name for FTS debris on this object.";
    return;
  }
  const sample = await ensureInFlightSample();
  const timeS = sample?.time_s ?? simDraft.timeS;
  if (timeS == null || Number.isNaN(Number(timeS))) {
    els.progress.textContent = "Pick an in-flight time on the source trajectory.";
    return;
  }
  if (sample && Number(sample.alt_m) < 80) {
    els.progress.textContent = `FTS state is at ${Number(sample.alt_m).toFixed(0)} m — pick an in-flight time.`;
    return;
  }
  setBusy(true);
  els.progress.textContent = "Propagating FTS debris…";
  try {
    const result = await invoke("simulate_fts", ipcArgs({
      spec: {
        source_track_id: simDraft.trackId,
        time_s: Number(timeS),
        catalog_id: ensureCatalogId(),
        object_id: dest.objectId,
        object_name: null,
        mode_name: dest.modeName,
        seed: simDraft.seed || 1,
      },
    }));
    await ingestSimResult(result, `FTS · ${result.tracks.length} debris impacts`);
  } catch (err) {
    els.progress.textContent = String(err);
  } finally {
    setBusy(false);
    render();
  }
}

async function runSimNav() {
  readSimForm();
  const trackId = simDraft.trackId || preferredSimTrackId();
  if (!trackId) {
    els.progress.textContent = "Pick a source trajectory first.";
    return;
  }
  simDraft.trackId = trackId;
  const dest = simDestination();
  if (!dest.objectId || !dest.modeName) {
    els.progress.textContent = "Enter a mode name for nav + FTS on this object.";
    return;
  }
  const sample = await ensureInFlightSample();
  let times = Array.isArray(simDraft.nav.times) ? simDraft.nav.times.slice() : [];
  const sliderTime = Number(simHost()?.querySelector("[data-sim='time']")?.value);
  const fallback = sample?.time_s ?? simDraft.timeS ?? (Number.isFinite(sliderTime) ? sliderTime : null);
  if (!times.length && fallback != null && !Number.isNaN(Number(fallback))) {
    times = [Number(fallback)];
  }
  times = await pickInFlightTimes(times);
  if (!times.length && sample && Number(sample.alt_m) >= 80) {
    times = [Number(sample.time_s)];
  }
  if (!times.length) {
    els.progress.textContent = "No in-flight times — move the slider off the pad/landing, or Fill span.";
    return;
  }
  const maxG = Number(simDraft.nav.maxG);
  const dur = Number(simDraft.nav.durationS);
  setBusy(true);
  els.progress.textContent = `Nav + FTS at ${times.length} time(s)…`;
  try {
    const result = await invoke("simulate_nav_failure", ipcArgs({
      spec: {
        source_track_id: trackId,
        times_s: times,
        catalog_id: ensureCatalogId(),
        object_id: dest.objectId,
        object_name: null,
        mode_name: dest.modeName,
        max_g: Number.isFinite(maxG) && maxG >= 0.1 ? maxG : 5,
        turn_duration_s: Number.isFinite(dur) && dur >= 0.1 ? dur : 5,
        turn_side: simDraft.nav.side || "both",
        sustain_speed: simDraft.nav.sustain !== false,
        seed: simDraft.seed || 1,
      },
    }));
    const debrisN = (result.tracks || []).filter((t) => (t.weight ?? 1) > 0).length;
    await ingestSimResult(
      result,
      `Nav + FTS · ${debrisN} debris impacts (${result.tracks.length} total)`,
    );
  } catch (err) {
    els.progress.textContent = String(err);
  } finally {
    setBusy(false);
    render();
  }
}

async function runGenerate(objectId) {
  const obj = riskModel.objects.find((o) => o.id === objectId);
  if (!obj) return;
  const card = document.querySelector(`[data-object-id="${objectId}"]`);
  const modeName = fieldText(card?.querySelector("[data-role='load-mode-name']"))
    || objectLoadName.get(objectId)
    || "";
  if (!modeName) {
    els.progress.textContent = "Enter a mode name, then generate.";
    card?.querySelector("[data-role='load-mode-name']")?.focus();
    return;
  }
  objectLoadName.set(objectId, modeName);
  const draft = draftFor(obj);
  const spec = {
    launch_lat: Number(draft.launch_lat),
    launch_lon: Number(draft.launch_lon),
    launch_alt_m: Number(draft.launch_alt_m) || 0,
    aim_lat: Number(draft.aim_lat),
    aim_lon: Number(draft.aim_lon),
    aim_alt_m: Number(draft.aim_alt_m) || 0,
    ballistic_coeff: Number(draft.ballistic_coeff),
    burnout_alt_m: Number(draft.burnout_alt_m),
  };
  if (!Number.isFinite(spec.launch_lat) || !Number.isFinite(spec.aim_lat)) {
    els.progress.textContent = "Set launch and aim (type coordinates or pick on the globe).";
    return;
  }
  setBusy(true);
  els.progress.textContent = missionWind.type === "historical" ? "Fetching regional winds…" : "Generating…";
  try {
    const result = await invoke("generate_trajectories", ipcArgs({ objectId, spec, modeName }));
    for (const track of result.tracks) {
      tracks.set(track.id, track);
    }
    addTracks(result.tracks);
    objectMethod.set(objectId, "generate");
    await refreshRisk();
    if (result.tracks.length) {
      fitAll(result.tracks.map((t) => t.bounds).filter(Boolean));
      selectTrack(result.tracks[0].id);
    }
    await refreshOverlays({ immediate: true, force: true });
    els.statTime.textContent = result.elapsed_ms ? `${result.elapsed_ms} ms` : "—";
    applyMissionWind(riskModel.wind);
    const windNote = riskModel.wind?.type === "historical" && riskModel.wind.source
      ? ` · ${riskModel.wind.source}`
      : riskModel.wind?.type === "constant"
        ? ` · ${riskModel.wind.speed_mps} m/s from ${riskModel.wind.from_deg}°`
        : "";
    els.progress.textContent = `Generated 1 traj into ${modeName}${windNote}`;
    await refreshMission();
  } catch (err) {
    els.progress.textContent = String(err);
  } finally {
    setBusy(false);
    render();
  }
}

async function runRocketPy(objectId) {
  const obj = riskModel.objects.find((o) => o.id === objectId);
  if (!obj) return;
  const modeName = requireModeNameFor(objectId);
  if (!modeName) return;
  const spec = mergeRocketPySpec(rocketpyDraftFor(obj));
  generateEngine.set(objectId, "rocketpy");
  objectMethod.set(objectId, "generate");
  setBusy(true);
  els.progress.textContent = "Flying 6DOF…";
  try {
    const result = await invoke("generate_rocketpy", ipcArgs({ objectId, spec, modeName }));
    for (const track of result.tracks || []) tracks.set(track.id, track);
    addTracks(result.tracks || []);
    await refreshRisk();
    if (result.tracks?.length) {
      fitAll(result.tracks.map((t) => t.bounds).filter(Boolean));
      selectTrack(result.tracks[0].id);
    }
    await refreshOverlays({ immediate: true, force: true });
    els.statTime.textContent = result.elapsed_ms ? `${result.elapsed_ms} ms` : "—";
    const apo = Number(result.apogee_m);
    const impact = Number(result.impact_time_s);
    const apoNote = Number.isFinite(apo) ? ` · apogee ${(apo / 1000).toFixed(1)} km` : "";
    const tNote = Number.isFinite(impact) ? ` · ${impact.toFixed(1)} s` : "";
    els.progress.textContent = `6DOF 1 traj into ${modeName}${apoNote}${tNote}`;
    await refreshMission();
  } catch (err) {
    els.progress.textContent = String(err);
    refreshRocketPyStatus();
  } finally {
    setBusy(false);
    render();
    if (rocketpyOpenId === objectId) paintRocketPyOverlay();
  }
}

function flyToImpactsOrTracks(trackList) {
  const grid = kdeBounds();
  if (grid) {
    flyToBounds(grid);
    return;
  }
  const hits = impactBounds();
  if (hits) {
    flyToBounds(hits);
    return;
  }
  if (trackList?.length) {
    fitAll(trackList.map((t) => t.bounds).filter(Boolean));
    return;
  }
  const bounds = boatBounds(boats.filter((b) => b.visible !== false));
  if (bounds) flyToBounds(bounds);
}

function assignedPointKey() {
  return [...tracks.values()]
    .filter((t) => t.object_id != null && t.failure_mode_id != null)
    .map((t) => {
      const b = t.bounds || {};
      return `${t.id}:${t.visible !== false}:${t.show_path !== false}:${t.weight}:${b.west}:${b.south}:${b.east}:${b.north}:${b.min_alt}:${b.max_alt}`;
    })
    .sort()
    .join("|");
}

function assignedWeightKey() {
  return [...tracks.values()]
    .filter((t) => t.object_id != null && t.failure_mode_id != null)
    .map((t) => `${t.id}:${t.probability ?? 0}`)
    .sort()
    .join("|");
}

function setImpactStatus(text) {
  lastImpactSummary = text;
  if (els.kdeStatus) els.kdeStatus.textContent = text;
}

function cancelKdeWork() {
  if (kdeTimer) {
    clearTimeout(kdeTimer);
    kdeTimer = null;
  }
  kdeSeq += 1;
}

function scheduleKde() {
  cancelKdeWork();
  const scheduled = kdeSeq;
  kdeTimer = setTimeout(() => {
    kdeTimer = null;
    if (scheduled !== kdeSeq) return;
    runKde(scheduled);
  }, KDE_IDLE_MS);
}

function terminateHullPoints() {
  const pts = [];
  for (const t of tracks.values()) {
    if (t.visible === false) continue;
    if (!Number.isFinite(t.breakup_lon) || !Number.isFinite(t.breakup_lat)) continue;
    pts.push({ lon: t.breakup_lon, lat: t.breakup_lat });
  }
  return pts;
}

function syncTerminateHull() {
  setTerminateHull(terminateHullPoints());
  setTerminateHullVisible(els.showTerminateBoundary?.checked === true);
}

async function refreshOverlays(opts = {}) {
  syncTerminateHull();
  const pointKey = assignedPointKey();
  const weightKey = assignedWeightKey();
  if (!pointKey) {
    impactSeq += 1;
    cancelKdeWork();
    lastPointKey = "";
    lastWeightKey = "";
    lastImpactSummary = "";
    setImpactPoints([]);
    await setKdeGrid(null);
    clearBoatScores();
    setImpactStatus("");
    return;
  }
  const pointsChanged = opts.force || pointKey !== lastPointKey;
  const weightsChanged = weightKey !== lastWeightKey;
  lastPointKey = pointKey;
  lastWeightKey = weightKey;
  if (pointsChanged) {
    await setKdeGrid(null);
    await extractAndShowImpacts();
  }
  if (pointsChanged || weightsChanged || opts.immediate) {
    if (opts.immediate) {
      cancelKdeWork();
      await runKde(kdeSeq);
    } else {
      scheduleKde();
    }
  }
}

async function extractAndShowImpacts() {
  const seq = ++impactSeq;
  try {
    const result = await invoke("extract_impacts", ipcArgs({ thresholdM: 0 }));
    if (seq !== impactSeq) return;
    setImpactPoints(result.impacts);
    setImpactVisible(els.showImpacts?.checked !== false);
    setImpactStatus(`${result.impacts.length} impacts, ${result.missing.length} no-impact`);
  } catch (err) {
    if (seq !== impactSeq) return;
    setImpactStatus(`Impact extract failed: ${String(err)}`);
  }
}

async function runKde(seq) {
  if (els.kdeStatus && lastImpactSummary) {
    els.kdeStatus.textContent = `${lastImpactSummary} · computing heatmap`;
  }
  try {
    const result = await invoke("compute_kde", ipcArgs({ objectId: null, thresholdM: 0 }));
    if (seq !== kdeSeq) return;
    lastWeightKey = assignedWeightKey();
    setImpactPoints(result.impacts);
    setImpactVisible(els.showImpacts?.checked !== false);
    lastKdeGrid = result.grid || null;
    await setKdeGrid(result.grid);
    if (seq !== kdeSeq) return;
    setKdeVisible(els.showKde?.checked !== false);
    if (result.boats?.length) applyScoredBoats(result.boats);
    else applyScoredBoats(scoreBoatsAgainstGrid(boats, lastKdeGrid));
    const scored = boats.filter((b) => Number(b.p_hit) > 0).length;
    const bw = result.bandwidth_east_m
      ? `hE ${result.bandwidth_east_m.toFixed(0)} m × hN ${result.bandwidth_north_m.toFixed(0)} m`
      : (result.impacts.length ? "heatmap empty" : "no impacts to contour");
    const boatNote = boats.length ? `, ${scored}/${boats.length} boats in debris field` : "";
    setImpactStatus(`${result.impacts.length} impacts, ${result.missing} no-impact, mass ${formatProb(result.mass)}, ${bw}${boatNote}`);
  } catch (err) {
    if (seq !== kdeSeq) return;
    const msg = String(err);
    setImpactStatus(lastImpactSummary ? `${lastImpactSummary} · KDE failed: ${msg}` : `KDE failed: ${msg}`);
  }
}

function setupSplitter() {
  const sidebar = document.getElementById("sidebar");
  const splitter = document.getElementById("splitter");
  let dragging = false;
  splitter.addEventListener("mousedown", () => {
    dragging = true;
    document.body.style.cursor = "col-resize";
  });
  window.addEventListener("mouseup", () => {
    dragging = false;
    document.body.style.cursor = "";
    resizeGlobe();
  });
  window.addEventListener("mousemove", (ev) => {
    if (!dragging) return;
    sidebar.style.width = `${Math.min(560, Math.max(260, ev.clientX))}px`;
    resizeGlobe();
  });
  els.imagery?.addEventListener("change", (ev) => {
    setImagery(ev.target.value);
    persistMissionUi();
  });
}

function currentMissionUi() {
  return {
    impact_threshold_m: 0,
    imagery: els.imagery?.value || "satellite",
    show_impacts: Boolean(els.showImpacts?.checked),
    show_kde: Boolean(els.showKde?.checked),
    show_boats: Boolean(els.showBoats?.checked),
    show_iip_boundary: Boolean(els.showIipBoundary?.checked),
    show_terminate_boundary: Boolean(els.showTerminateBoundary?.checked),
    kde_object_name: null,
  };
}

async function persistMissionUi() {
  try {
    mission = await invoke("set_mission_ui", ipcArgs({ ui: currentMissionUi() }));
    renderMission();
  } catch (err) {
    els.progress.textContent = String(err);
  }
}

function renderMission() {
  if (els.missionName && document.activeElement !== els.missionName) {
    els.missionName.value = mission.name || "Untitled mission";
  }
  els.missionDirty?.classList.toggle("hidden", !mission.dirty);
  els.missionPath.textContent = mission.path || "Not saved yet — Save writes a .fauxrrt file";
}

function confirmDiscard() {
  if (!mission.dirty || mission.path) return true;
  return window.confirm("Discard this unsaved mission?");
}

async function runMission(cmd, checkDiscard) {
  if (checkDiscard && !confirmDiscard()) return;
  setBusy(true);
  try {
    const snap = await invoke(cmd);
    await applySnapshot(snap, cmd.startsWith("open") || cmd === "new_mission");
  } catch (err) {
    els.progress.textContent = String(err);
  } finally {
    setBusy(false);
  }
}

async function openRecent(path) {
  if (!path || !confirmDiscard()) return;
  setBusy(true);
  try {
    const snap = await invoke("open_mission_path", ipcArgs({ path }));
    await applySnapshot(snap, true);
  } catch (err) {
    els.progress.textContent = String(err);
  } finally {
    setBusy(false);
  }
}

async function applySnapshot(snap, fit) {
  if (!snap) return;
  mission = snap.mission || mission;
  riskModel = snap.risk || { objects: [], unassigned: [], wind: { type: "off" } };
  applyMissionWind(riskModel.wind);
  tracks.clear();
  selectedId = null;
  selectedBoatId = null;
  hoveredBoatId = null;
  lastKdeGrid = null;
  generateDrafts.clear();
  generateEngine.clear();
  rocketpyDrafts.clear();
  objectMethod.clear();
  objectLoadName.clear();
  closeRocketPyBuilder({ render: false });
  simDraft.trackId = 0;
  simDraft.timeS = null;
  simDraft.sample = null;
  simDraft.nav.times = [];
  setStateMarker(null);
  syncCatalogs(snap.risk);
  expandedObjectId = undefined;
  pendingExpandLast = false;
  cancelGlobePick();
  pickKind = null;
  impactSeq += 1;
  cancelKdeWork();
  lastPointKey = "";
  lastWeightKey = "";
  lastImpactSummary = "";
  clearTracks();
  for (const track of snap.tracks || []) {
    tracks.set(track.id, track);
  }
  addTracks(snap.tracks || []);
  applyScoredBoats(snap.boats || []);
  highlightBoat(null);
  applyRiskToTracks();
  applyMissionUi(snap.ui);
  renderMission();
  render();
  applyMissionUi(snap.ui);
  await refreshOverlays();
  if (snap.errors?.length) {
    els.progress.textContent = `${snap.errors.length} trajectory file(s) missing`;
    console.warn(snap.errors);
  } else if (fit && (snap.tracks?.length || boats.length)) {
    flyToImpactsOrTracks(snap.tracks.filter((t) => t.visible));
    els.progress.textContent = mission.path ? `Opened ${mission.name}` : "";
  } else if (fit) {
    els.progress.textContent = "";
  }
}

function applyMissionUi(ui) {
  if (!ui) return;
  if (els.showImpacts && ui.show_impacts != null) {
    els.showImpacts.checked = ui.show_impacts;
    setImpactVisible(ui.show_impacts);
  }
  if (els.showKde && ui.show_kde != null) {
    els.showKde.checked = ui.show_kde;
    setKdeVisible(ui.show_kde);
  }
  if (els.showBoats && ui.show_boats != null) {
    els.showBoats.checked = ui.show_boats;
    setBoatsVisible(ui.show_boats);
  }
  if (els.showIipBoundary && ui.show_iip_boundary != null) {
    els.showIipBoundary.checked = ui.show_iip_boundary;
    setIipHullVisible(ui.show_iip_boundary);
  }
  if (els.showTerminateBoundary && ui.show_terminate_boundary != null) {
    els.showTerminateBoundary.checked = ui.show_terminate_boundary;
    setTerminateHullVisible(ui.show_terminate_boundary);
  }
  if (els.imagery && ui.imagery) {
    els.imagery.value = ui.imagery;
    setImagery(ui.imagery);
  }
}

async function refreshMission() {
  try {
    mission = await invoke("get_mission");
    renderMission();
  } catch {
    /* previewed outside the Tauri shell */
  }
}
