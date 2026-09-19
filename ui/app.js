const invoke = window.__TAURI__.core.invoke;

const tracks = new Map();
let selectedId = null;
let polylineCollection = null;
let viewer = null;

function $(id) {
  return document.getElementById(id);
}

function setStatus(text) {
  $("status").textContent = text;
}

function renderStats() {
  const list = [...tracks.values()];
  const points = list.reduce((n, t) => n + t.pointCount, 0);
  const display = list.reduce((n, t) => n + t.displayCount, 0);
  $("stats").textContent = list.length
    ? `${list.length} tracks · ${points.toLocaleString()} states · ${display.toLocaleString()} drawn`
    : "";
}

function roleLabel(schema) {
  if (schema.frame === "ecef") return "ECEF";
  return schema.alt_col == null ? "LLA (no alt)" : "LLA";
}

function renderTracks() {
  const root = $("track-list");
  const list = [...tracks.values()];
  if (!list.length) {
    root.innerHTML = '<div class="empty">No trajectories loaded.</div>';
    $("schema-block").hidden = true;
    renderStats();
    return;
  }
  root.innerHTML = "";
  for (const track of list) {
    const el = document.createElement("div");
    el.className = "track" + (track.id === selectedId ? " active" : "");
    el.innerHTML = `
      <span class="swatch" style="background:${track.color}"></span>
      <div>
        <b>${track.name}</b>
        <span>${track.pointCount.toLocaleString()} states · ${roleLabel(track.schema)} · ${track.loadMs.toFixed(1)} ms</span>
      </div>
      <input type="checkbox" ${track.visible ? "checked" : ""} data-id="${track.id}" />
    `;
    el.addEventListener("click", (e) => {
      if (e.target.tagName === "INPUT") return;
      selectTrack(track.id);
      flyTo(track);
    });
    el.querySelector("input").addEventListener("change", async (e) => {
      track.visible = e.target.checked;
      await invoke("set_visible", { id: track.id, visible: track.visible });
      const line = track.polyline;
      if (line) line.show = track.visible;
    });
    root.appendChild(el);
  }
  renderStats();
}

function columnOptions(schema, selected) {
  const opts = ['<option value="">—</option>']
    .concat(
      schema.columns.map(
        (c) =>
          `<option value="${c.index}" ${String(selected) === String(c.index) ? "selected" : ""}>${c.index}: ${c.name}</option>`
      )
    )
    .join("");
  return opts;
}

function renderSchema(track) {
  $("schema-block").hidden = false;
  $("schema-confidence").textContent = `${Math.round(track.schema.confidence * 100)}%`;
  $("schema-summary").innerHTML = `
    <span class="pill">${track.schema.delimiter}</span>
    <span class="pill">${track.schema.has_header ? "header" : "no header"}</span>
    <span class="pill">${roleLabel(track.schema)}</span>
    ${track.schema.ecef_scale === 1000 ? '<span class="pill">ECEF km → m</span>' : ""}
  `;
  $("schema-notes").textContent = (track.schema.notes || []).join(" · ");
  const s = track.schema;
  $("schema-map").innerHTML = `
    <label>Frame
      <select id="map-frame">
        <option value="lla" ${s.frame === "lla" ? "selected" : ""}>Lat / Lon / Alt</option>
        <option value="ecef" ${s.frame === "ecef" ? "selected" : ""}>ECEF X / Y / Z</option>
      </select>
    </label>
    <label>Time <select id="map-time">${columnOptions(s, s.time_col)}</select></label>
    <label>Lat <select id="map-lat">${columnOptions(s, s.lat_col)}</select></label>
    <label>Lon <select id="map-lon">${columnOptions(s, s.lon_col)}</select></label>
    <label>Alt <select id="map-alt">${columnOptions(s, s.alt_col)}</select></label>
    <label>X <select id="map-x">${columnOptions(s, s.x_col)}</select></label>
    <label>Y <select id="map-y">${columnOptions(s, s.y_col)}</select></label>
    <label>Z <select id="map-z">${columnOptions(s, s.z_col)}</select></label>
  `;
}

function selectedInt(id) {
  const v = $(id).value;
  return v === "" ? null : Number(v);
}

function selectTrack(id) {
  selectedId = id;
  const track = tracks.get(id);
  if (track) renderSchema(track);
  renderTracks();
}

function flyTo(track) {
  const b = track.bounds;
  viewer.camera.flyTo({
    destination: Cesium.Rectangle.fromDegrees(
      Math.min(b.west, b.east) - 0.4,
      Math.min(b.south, b.north) - 0.4,
      Math.max(b.west, b.east) + 0.4,
      Math.max(b.south, b.north) + 0.4
    ),
    duration: 0.8,
  });
}

function fitAll() {
  const visible = [...tracks.values()].filter((t) => t.visible);
  if (!visible.length) return;
  let west = 180, east = -180, south = 90, north = -90;
  for (const t of visible) {
    west = Math.min(west, t.bounds.west);
    east = Math.max(east, t.bounds.east);
    south = Math.min(south, t.bounds.south);
    north = Math.max(north, t.bounds.north);
  }
  viewer.camera.flyTo({
    destination: Cesium.Rectangle.fromDegrees(west, south, east, north),
    duration: 0.9,
  });
}

async function addTracks(list) {
  for (const meta of list) {
    const packed = await invoke("track_positions", { id: meta.id });
    const positions = Cesium.Cartesian3.fromDegreesArrayHeights(packed);
    const polyline = polylineCollection.add({
      positions,
      width: 2.2,
      material: Cesium.Material.fromType("Color", {
        color: Cesium.Color.fromCssColorString(trackColor(meta.color)),
      }),
    });
    tracks.set(meta.id, { ...meta, polyline, visible: true });
    selectedId = meta.id;
  }
  renderTracks();
  if (selectedId) renderSchema(tracks.get(selectedId));
  fitAll();
}

function trackColor(css) {
  return css;
}

async function run(label, fn) {
  setStatus(label);
  try {
    const result = await fn();
    if (result && result.length) {
      await addTracks(result);
      setStatus(`Loaded ${result.length} track${result.length === 1 ? "" : "s"}`);
    } else {
      setStatus("Ready");
    }
  } catch (err) {
    const message = String(err);
    if (!/cancelled/i.test(message)) setStatus(message);
    else setStatus("Ready");
  }
}

function initGlobe() {
  const imagery = new Cesium.UrlTemplateImageryProvider({
    url: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
    maximumLevel: 19,
    credit: "Tiles © Esri",
  });
  viewer = new Cesium.Viewer("globe", {
    animation: false,
    timeline: false,
    baseLayerPicker: false,
    geocoder: false,
    homeButton: true,
    navigationHelpButton: false,
    fullscreenButton: false,
    infoBox: false,
    selectionIndicator: false,
    sceneModePicker: true,
    terrainProvider: new Cesium.EllipsoidTerrainProvider(),
    baseLayer: new Cesium.ImageryLayer(imagery),
  });
  viewer.scene.globe.depthTestAgainstTerrain = false;
  viewer.scene.globe.baseColor = Cesium.Color.fromCssColorString("#0b1016");
  polylineCollection = viewer.scene.primitives.add(new Cesium.PolylineCollection());
}

function initSplitter() {
  const splitter = $("splitter");
  const sidebar = $("sidebar");
  let dragging = false;
  splitter.addEventListener("pointerdown", (e) => {
    dragging = true;
    splitter.setPointerCapture(e.pointerId);
  });
  splitter.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    sidebar.style.width = Math.max(260, Math.min(560, e.clientX)) + "px";
  });
  splitter.addEventListener("pointerup", () => {
    dragging = false;
  });
}

function clearGlobe() {
  polylineCollection.removeAll();
  tracks.clear();
  selectedId = null;
  renderTracks();
}

window.addEventListener("DOMContentLoaded", () => {
  initGlobe();
  initSplitter();
  $("btn-files").onclick = () => run("Loading files…", () => invoke("load_files"));
  $("btn-folder").onclick = () => run("Loading folder…", () => invoke("load_folder"));
  $("btn-demo").onclick = () => run("Generating demo tracks…", () => invoke("load_demo"));
  $("btn-fit").onclick = fitAll;
  $("btn-clear").onclick = async () => {
    await invoke("clear_tracks");
    clearGlobe();
    setStatus("Cleared");
  };
  $("btn-remap").onclick = async () => {
    if (selectedId == null) return;
    const track = tracks.get(selectedId);
    const schema = {
      ...track.schema,
      frame: $("map-frame").value,
      time_col: selectedInt("map-time"),
      lat_col: selectedInt("map-lat"),
      lon_col: selectedInt("map-lon"),
      alt_col: selectedInt("map-alt"),
      x_col: selectedInt("map-x"),
      y_col: selectedInt("map-y"),
      z_col: selectedInt("map-z"),
    };
    setStatus("Re-reading with new mapping…");
    try {
      const meta = await invoke("remap_track", { id: selectedId, schema });
      if (track.polyline) polylineCollection.remove(track.polyline);
      const packed = await invoke("track_positions", { id: meta.id });
      const polyline = polylineCollection.add({
        positions: Cesium.Cartesian3.fromDegreesArrayHeights(packed),
        width: 2.2,
        material: Cesium.Material.fromType("Color", {
          color: Cesium.Color.fromCssColorString(meta.color),
        }),
      });
      tracks.set(meta.id, { ...meta, polyline, visible: true });
      renderTracks();
      renderSchema(tracks.get(meta.id));
      flyTo(meta);
      setStatus("Remapped " + meta.name);
    } catch (err) {
      setStatus(String(err));
    }
  };
});
