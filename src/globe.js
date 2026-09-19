const tracks = new Map();
let viewer;
let polylines;
let arrowPrims;
let sitePoints;
let pickHandler;
let pickCallback = null;
let selectedId = null;
let loadGen = 0;
const ARROW_LIMIT = 24;
const materialCache = new Map();

function requestRender() {
  viewer?.scene?.requestRender();
}

function colorMaterial(Cesium, color) {
  const key = `${color.red.toFixed(3)},${color.green.toFixed(3)},${color.blue.toFixed(3)},${color.alpha.toFixed(3)}`;
  let mat = materialCache.get(key);
  if (!mat) {
    mat = Cesium.Material.fromType("Color", { color });
    materialCache.set(key, mat);
  }
  return mat;
}

function highZoomImagery(Cesium) {
  return new Cesium.UrlTemplateImageryProvider({
    url: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
    credit: "Tiles © Esri",
    maximumLevel: 19,
    enablePickFeatures: false,
  });
}

function streetImagery(Cesium) {
  return new Cesium.UrlTemplateImageryProvider({
    url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
    credit: "© OpenStreetMap contributors",
    maximumLevel: 19,
    enablePickFeatures: false,
  });
}

export async function createGlobe(element) {
  const Cesium = window.Cesium;
  let baseLayer = new Cesium.ImageryLayer(highZoomImagery(Cesium));

  viewer = new Cesium.Viewer(element, {
    animation: false,
    timeline: false,
    baseLayerPicker: false,
    geocoder: false,
    homeButton: true,
    navigationHelpButton: false,
    sceneModePicker: true,
    fullscreenButton: false,
    infoBox: false,
    selectionIndicator: false,
    terrainProvider: new Cesium.EllipsoidTerrainProvider(),
    baseLayer,
    useBrowserRecommendedResolution: true,
    requestRenderMode: true,
    maximumRenderTimeChange: Infinity,
  });
  viewer.resolutionScale = 1;
  viewer.scene.globe.depthTestAgainstTerrain = false;
  viewer.scene.globe.enableLighting = false;
  viewer.scene.fog.enabled = false;
  viewer.scene.skyAtmosphere.show = true;
  viewer.scene.globe.showGroundAtmosphere = false;
  viewer.scene.globe.maximumScreenSpaceError = 2;
  viewer.scene.globe.tileCacheSize = 500;
  viewer.scene.requestRenderMode = true;
  viewer.scene.maximumRenderTimeChange = Infinity;
  viewer.clock.shouldAnimate = false;
  polylines = viewer.scene.primitives.add(new Cesium.PolylineCollection());
  arrowPrims = viewer.scene.primitives.add(new Cesium.PrimitiveCollection());
  sitePoints = viewer.scene.primitives.add(new Cesium.PointPrimitiveCollection());
  bindBoatPicking();
  bindSiteDragging();
  window.addEventListener("resize", resizeGlobe);
  return viewer;
}

export function beginGlobePick(onPick) {
  const Cesium = window.Cesium;
  if (!viewer) return;
  pickCallback = onPick;
  if (!pickHandler) {
    pickHandler = new Cesium.ScreenSpaceEventHandler(viewer.scene.canvas);
    pickHandler.setInputAction((click) => {
      if (!pickCallback || siteDrag) return;
      const cartesian = viewer.camera.pickEllipsoid(click.position, viewer.scene.globe.ellipsoid);
      if (!cartesian) return;
      const carto = Cesium.Cartographic.fromCartesian(cartesian);
      pickCallback({
        lat: Cesium.Math.toDegrees(carto.latitude),
        lon: Cesium.Math.toDegrees(carto.longitude),
        alt: carto.height || 0,
      });
    }, Cesium.ScreenSpaceEventType.LEFT_CLICK);
  }
}

export function cancelGlobePick() {
  pickCallback = null;
}

let siteDrag = null;
let siteChangeHandler = null;
let lastLaunchSite = null;
let lastAimSite = null;

export function setSiteChangeHandler(fn) {
  siteChangeHandler = fn;
}

function emitSiteChange(kind, lat, lon, alt, dragging) {
  if (kind === "launch") lastLaunchSite = { lat, lon, alt };
  if (kind === "aim") lastAimSite = { lat, lon, alt };
  siteChangeHandler?.({ kind, lat, lon, alt, dragging: Boolean(dragging) });
}

function pickNearestId(collection, position, getId, maxPx) {
  if (!collection || !viewer) return null;
  const limit = maxPx * maxPx;
  let best = null;
  let bestD = limit;
  const n = collection.length;
  for (let i = 0; i < n; i++) {
    const p = collection.get(i);
    if (p.show === false) continue;
    const id = getId(p);
    if (id == null) continue;
    const win = viewer.scene.cartesianToCanvasCoordinates(p.position);
    if (!win) continue;
    const dx = win.x - position.x;
    const dy = win.y - position.y;
    const d = dx * dx + dy * dy;
    if (d <= bestD) {
      bestD = d;
      best = id;
    }
  }
  return best;
}

function pickSiteKind(position) {
  return pickNearestId(sitePoints, position, (p) => p.id?.siteKind || null, 14);
}

function bindSiteDragging() {
  const Cesium = window.Cesium;
  const handler = new Cesium.ScreenSpaceEventHandler(viewer.scene.canvas);
  const controller = viewer.scene.screenSpaceCameraController;

  handler.setInputAction((click) => {
    if (pickCallback) return;
    const kind = pickSiteKind(click.position);
    if (!kind) return;
    siteDrag = { kind };
    controller.enableInputs = false;
    viewer.canvas.style.cursor = "grabbing";
  }, Cesium.ScreenSpaceEventType.LEFT_DOWN);

  handler.setInputAction((move) => {
    if (!siteDrag) {
      viewer.canvas.style.cursor = pickSiteKind(move.endPosition) ? "grab" : "";
      return;
    }
    const cartesian = viewer.camera.pickEllipsoid(move.endPosition, viewer.scene.globe.ellipsoid);
    if (!cartesian) return;
    const carto = Cesium.Cartographic.fromCartesian(cartesian);
    const lat = Cesium.Math.toDegrees(carto.latitude);
    const lon = Cesium.Math.toDegrees(carto.longitude);
    const prev = siteDrag.kind === "aim" ? lastAimSite : lastLaunchSite;
    const alt = prev?.alt ?? 0;
    moveSiteMarker(siteDrag.kind, lat, lon, alt);
    emitSiteChange(siteDrag.kind, lat, lon, alt, true);
  }, Cesium.ScreenSpaceEventType.MOUSE_MOVE);

  handler.setInputAction(() => {
    if (!siteDrag) return;
    const kind = siteDrag.kind;
    siteDrag = null;
    controller.enableInputs = true;
    viewer.canvas.style.cursor = "";
    const site = kind === "aim" ? lastAimSite : lastLaunchSite;
    if (site) emitSiteChange(kind, site.lat, site.lon, site.alt, false);
  }, Cesium.ScreenSpaceEventType.LEFT_UP);
}

function moveSiteMarker(kind, lat, lon, alt) {
  if (!sitePoints) return;
  const Cesium = window.Cesium;
  const pos = Cesium.Cartesian3.fromDegrees(lon, lat, alt || 0);
  const n = sitePoints.length;
  for (let i = 0; i < n; i++) {
    const p = sitePoints.get(i);
    if (p.id?.siteKind === kind) {
      p.position = pos;
      requestRender();
      return;
    }
  }
}

let boatPoints;
let boatLines;
let lastBoats = [];
let boatsVisible = true;
let selectedBoatId = null;
let boatPickHandler = null;
let onBoatHover = null;
let onBoatClick = null;

export function setBoats(boats) {
  const Cesium = window.Cesium;
  if (!viewer) return;
  if (!boatPoints) {
    boatPoints = viewer.scene.primitives.add(new Cesium.PointPrimitiveCollection());
  }
  if (!boatLines) {
    boatLines = viewer.scene.primitives.add(new Cesium.PolylineCollection());
  }
  boatPoints.removeAll();
  boatLines.removeAll();
  lastBoats = boats || [];
  for (const boat of lastBoats) {
    if (boat.visible === false) continue;
    const selected = boat.id === selectedBoatId;
    const last = Cesium.Cartesian3.fromDegrees(boat.lon, boat.lat, Math.max(boat.alt_m || 0, 0));
    const pickId = { boatId: boat.id };
    const drawColor = Cesium.Color.fromCssColorString(boat.display_color || boat.color || "#3ecfc4");
    boatPoints.add({
      id: pickId,
      position: last,
      color: drawColor,
      pixelSize: selected ? 14 : boatSizePx(boat),
      outlineColor: Cesium.Color.WHITE,
      outlineWidth: selected ? 2 : 1,
      disableDepthTestDistance: Number.POSITIVE_INFINITY,
    });
    if (boat.estimate_lon != null && (boat.estimate_lon !== boat.lon || boat.estimate_lat !== boat.lat)) {
      boatPoints.add({
        id: pickId,
        position: Cesium.Cartesian3.fromDegrees(
          boat.estimate_lon,
          boat.estimate_lat,
          Math.max(boat.alt_m || 0, 0),
        ),
        color: drawColor.withAlpha(0.85),
        pixelSize: selected ? 10 : 7,
        outlineColor: Cesium.Color.WHITE.withAlpha(0.7),
        outlineWidth: 1,
        disableDepthTestDistance: Number.POSITIVE_INFINITY,
      });
    }
    if (boat.predict_lla?.length >= 6) {
      boatLines.add({
        id: pickId,
        positions: Cesium.Cartesian3.fromDegreesArrayHeights(boat.predict_lla),
        width: selected ? 3 : 2,
        material: Cesium.Material.fromType("Color", { color: drawColor.withAlpha(0.75) }),
      });
    }
    if (boat.uncertainty_m > 250) {
      const ring = circleDegrees(boat.lon, boat.lat, boat.uncertainty_m);
      boatLines.add({
        id: pickId,
        positions: Cesium.Cartesian3.fromDegreesArray(ring),
        width: 1,
        material: Cesium.Material.fromType("Color", { color: drawColor.withAlpha(0.4) }),
      });
    }
  }
  boatPoints.show = boatsVisible;
  boatLines.show = boatsVisible;
  requestRender();
}

export function setBoatsVisible(visible) {
  boatsVisible = visible;
  if (boatPoints) boatPoints.show = visible;
  if (boatLines) boatLines.show = visible;
  requestRender();
}

export function highlightBoat(id) {
  if (selectedBoatId === id) return;
  selectedBoatId = id;
  setBoats(lastBoats);
}

export function bindBoatInspect(handlers) {
  onBoatHover = handlers?.hover || null;
  onBoatClick = handlers?.click || null;
}

export function currentKdeGrid() {
  return lastGrid;
}

function bindBoatPicking() {
  const Cesium = window.Cesium;
  if (!viewer || boatPickHandler) return;
  boatPickHandler = new Cesium.ScreenSpaceEventHandler(viewer.scene.canvas);
  const boatIdAt = (position) => pickNearestId(boatPoints, position, (p) => p.id?.boatId ?? null, 16);
  boatPickHandler.setInputAction((move) => {
    if (pickCallback) {
      viewer.scene.canvas.style.cursor = "crosshair";
      onBoatHover?.(null, move.endPosition);
      return;
    }
    const id = boatIdAt(move.endPosition);
    viewer.scene.canvas.style.cursor = id != null ? "pointer" : "";
    onBoatHover?.(id, move.endPosition);
  }, Cesium.ScreenSpaceEventType.MOUSE_MOVE);
  boatPickHandler.setInputAction((click) => {
    if (pickCallback) return;
    const id = boatIdAt(click.position);
    if (id != null) onBoatClick?.(id);
  }, Cesium.ScreenSpaceEventType.LEFT_CLICK);
}

export function boatBounds(list) {
  const pts = (list || lastBoats).filter((b) => Number.isFinite(b.lon) && Number.isFinite(b.lat));
  if (!pts.length) return null;
  const lons = pts.flatMap((b) => [b.lon, b.estimate_lon].filter((v) => Number.isFinite(v)));
  const lats = pts.flatMap((b) => [b.lat, b.estimate_lat].filter((v) => Number.isFinite(v)));
  const bounds = {
    west: Math.min(...lons),
    south: Math.min(...lats),
    east: Math.max(...lons),
    north: Math.max(...lats),
  };
  if (bounds.east - bounds.west < 0.4) {
    bounds.west -= 0.4;
    bounds.east += 0.4;
  }
  if (bounds.north - bounds.south < 0.4) {
    bounds.south -= 0.4;
    bounds.north += 0.4;
  }
  return bounds;
}

function boatSizePx(boat) {
  const len = Number(boat.length_m) || 12;
  return Math.min(16, Math.max(7, 6 + len / 20));
}

function circleDegrees(lon, lat, radiusM, steps = 36) {
  const coords = [];
  const latRad = (lat * Math.PI) / 180;
  const dLat = radiusM / 111320;
  const dLon = radiusM / (111320 * Math.max(Math.cos(latRad), 0.15));
  for (let i = 0; i <= steps; i++) {
    const a = (i / steps) * Math.PI * 2;
    coords.push(lon + dLon * Math.sin(a), lat + dLat * Math.cos(a));
  }
  return coords;
}

let statePoints;
let stateLines;

export function setStateMarker(state) {
  const Cesium = window.Cesium;
  if (!viewer) return;
  if (!statePoints) {
    statePoints = viewer.scene.primitives.add(new Cesium.PointPrimitiveCollection());
  }
  if (!stateLines) {
    stateLines = viewer.scene.primitives.add(new Cesium.PolylineCollection());
  }
  statePoints.removeAll();
  stateLines.removeAll();
  if (!state || state.lon == null || state.lat == null) {
    requestRender();
    return;
  }
  const alt = Math.max(state.alt_m || 0, 0);
  const origin = Cesium.Cartesian3.fromDegrees(state.lon, state.lat, alt);
  statePoints.add({
    position: origin,
    color: Cesium.Color.fromCssColorString("#f0c14b"),
    pixelSize: 14,
    outlineColor: Cesium.Color.WHITE,
    outlineWidth: 2,
    disableDepthTestDistance: Number.POSITIVE_INFINITY,
  });
  const vx = Number(state.vx_ecef);
  const vy = Number(state.vy_ecef);
  const vz = Number(state.vz_ecef);
  const speed = Math.hypot(vx, vy, vz);
  if (speed > 1) {
    const len = Math.min(12_000, Math.max(2_500, speed * 2.5));
    const tip = new Cesium.Cartesian3(
      origin.x + (vx / speed) * len,
      origin.y + (vy / speed) * len,
      origin.z + (vz / speed) * len,
    );
    stateLines.add({
      positions: [origin, tip],
      width: 3,
      material: Cesium.Material.fromType("Color", {
        color: Cesium.Color.fromCssColorString("#f0c14b").withAlpha(0.9),
      }),
    });
  }
  requestRender();
}

export function setSiteMarkers(launch, aim) {
  const Cesium = window.Cesium;
  if (!sitePoints) return;
  lastLaunchSite = launch && Number.isFinite(Number(launch.lat)) && Number.isFinite(Number(launch.lon))
    ? { lat: Number(launch.lat), lon: Number(launch.lon), alt: Number(launch.alt) || 0 }
    : null;
  lastAimSite = aim && Number.isFinite(Number(aim.lat)) && Number.isFinite(Number(aim.lon))
    ? { lat: Number(aim.lat), lon: Number(aim.lon), alt: Number(aim.alt) || 0 }
    : null;
  if (siteDrag) {
    const site = siteDrag.kind === "aim" ? lastAimSite : lastLaunchSite;
    if (site) moveSiteMarker(siteDrag.kind, site.lat, site.lon, site.alt);
    return;
  }
  sitePoints.removeAll();
  const add = (site, color, siteKind) => {
    if (!site) return;
    sitePoints.add({
      id: { siteKind },
      position: Cesium.Cartesian3.fromDegrees(site.lon, site.lat, Math.max(site.alt || 0, 0)),
      color,
      pixelSize: 12,
      outlineColor: Cesium.Color.WHITE,
      outlineWidth: 2,
      disableDepthTestDistance: Number.POSITIVE_INFINITY,
    });
  };
  add(lastLaunchSite, Cesium.Color.fromCssColorString("#3d9eff"), "launch");
  add(lastAimSite, Cesium.Color.fromCssColorString("#e35d6a"), "aim");
  requestRender();
}

export function setImagery(kind) {
  if (!viewer) return;
  const Cesium = window.Cesium;
  const provider = kind === "streets" ? streetImagery(Cesium) : highZoomImagery(Cesium);
  viewer.imageryLayers.removeAll();
  viewer.imageryLayers.addImageryProvider(provider);
  requestRender();
}

export function resizeGlobe() {
  viewer?.resize();
  requestRender();
}

export function addTrack(meta) {
  addTrackNow(meta, { skipArrow: tracks.size >= ARROW_LIMIT && meta.id !== selectedId });
  applySceneBudget();
  syncArrows();
  requestRender();
}

function addTrackNow(meta, opts = {}) {
  const Cesium = window.Cesium;
  removeTrack(meta.id, { silent: true });
  if (meta.show_path === false || (meta.display_lla?.length || 0) < 6) return;
  const positions = Cesium.Cartesian3.fromDegreesArrayHeights(meta.display_lla);
  const color = Cesium.Color.fromCssColorString(meta.color || "#7cb8ff") || Cesium.Color.CYAN;
  const line = polylines.add({
    positions,
    width: meta.id === selectedId ? 4 : 2,
    material: colorMaterial(Cesium, color),
  });
  const wantArrow = !opts.skipArrow && (visibleCount() < ARROW_LIMIT || meta.id === selectedId);
  const arrowPrim = wantArrow ? addTravelArrow(Cesium, positions, color) : null;
  tracks.set(meta.id, { line, arrowPrim, meta, color, positions });
  if (meta.visible === false) {
    line.show = false;
    if (arrowPrim) arrowPrim.show = false;
  }
}

export function addTracks(list) {
  const items = (list || []).filter((t) => t.show_path !== false && (t.display_lla?.length || 0) >= 6);
  if (!items.length) return;
  const many = tracks.size + items.length > ARROW_LIMIT;
  loadGen += 1;
  const gen = loadGen;
  let i = 0;
  const chunkSize = many ? 64 : items.length;

  const step = () => {
    if (gen !== loadGen) return;
    const end = Math.min(i + chunkSize, items.length);
    for (; i < end; i++) addTrackNow(items[i], { skipArrow: many });
    requestRender();
    if (i < items.length) {
      requestAnimationFrame(step);
      return;
    }
    applySceneBudget();
    syncArrows();
    requestRender();
  };
  step();
}

export function removeTrack(id, opts = {}) {
  const item = tracks.get(id);
  if (!item) return;
  polylines.remove(item.line);
  if (item.arrowPrim && arrowPrims) arrowPrims.remove(item.arrowPrim);
  tracks.delete(id);
  if (!opts.silent) {
    applySceneBudget();
    requestRender();
  }
}

export function clearTracks() {
  loadGen += 1;
  polylines.removeAll();
  arrowPrims?.removeAll();
  tracks.clear();
  materialCache.clear();
  lastTerminatePts = [];
  rebuildTerminateHull();
  clearRiskOverlay();
  if (boatPoints) boatPoints.removeAll();
  if (boatLines) boatLines.removeAll();
  lastBoats = [];
  applySceneBudget();
  requestRender();
}

export function setVisible(id, visible) {
  const item = tracks.get(id);
  if (!item) return;
  item.line.show = visible;
  if (item.arrowPrim) item.arrowPrim.show = visible;
  item.meta.visible = visible;
  syncArrows();
  requestRender();
}

export function highlight(id) {
  if (selectedId === id) {
    syncArrows();
    requestRender();
    return;
  }
  const prev = tracks.get(selectedId);
  if (prev) prev.line.width = 2;
  selectedId = id;
  const item = tracks.get(id);
  if (item) item.line.width = 4;
  syncArrows();
  requestRender();
}

function visibleCount() {
  let n = 0;
  for (const item of tracks.values()) {
    if (item.meta.visible !== false) n += 1;
  }
  return n;
}

function applySceneBudget() {
  if (!viewer) return;
  const n = tracks.size;
  if (n > 800) {
    viewer.resolutionScale = 1;
    viewer.scene.globe.maximumScreenSpaceError = 4;
  } else if (n > 120) {
    viewer.resolutionScale = 1;
    viewer.scene.globe.maximumScreenSpaceError = 3;
  } else {
    viewer.resolutionScale = 1;
    viewer.scene.globe.maximumScreenSpaceError = 2;
  }
}

function syncArrows() {
  const Cesium = window.Cesium;
  if (!arrowPrims) return;
  const showAll = visibleCount() <= ARROW_LIMIT;
  for (const [id, item] of tracks) {
    const want = item.meta.visible !== false && (showAll || id === selectedId);
    if (want && !item.arrowPrim) {
      item.arrowPrim = addTravelArrow(Cesium, item.positions, item.color);
    } else if (!want && item.arrowPrim) {
      arrowPrims.remove(item.arrowPrim);
      item.arrowPrim = null;
    }
  }
}

export function flyToBounds(bounds) {
  if (!bounds || !viewer) return;
  const Cesium = window.Cesium;
  const rectangle = Cesium.Rectangle.fromDegrees(
    bounds.west,
    bounds.south,
    bounds.east,
    bounds.north,
  );
  const padded = padRectangle(rectangle);
  viewer.camera.flyTo({
    destination: padded,
    duration: 0.8,
  });
}

export function fitAll(boundList) {
  const visible = boundList.filter(Boolean);
  if (!visible.length) return;
  const bounds = {
    west: Math.min(...visible.map((b) => b.west)),
    south: Math.min(...visible.map((b) => b.south)),
    east: Math.max(...visible.map((b) => b.east)),
    north: Math.max(...visible.map((b) => b.north)),
  };
  if (bounds.east - bounds.west < 0.05) {
    bounds.west -= 0.25;
    bounds.east += 0.25;
  }
  if (bounds.north - bounds.south < 0.05) {
    bounds.south -= 0.25;
    bounds.north += 0.25;
  }
  flyToBounds(bounds);
}

let impactPoints;
let kdePrim;
let lastImpacts = [];
let lastGrid = null;
let lastKdeCanvas = null;
let iipHullEntity = null;
let terminateHullEntity = null;
let lastTerminatePts = [];
let showIipHull = false;
let showTerminateHull = false;

function uniqueLonLat(points) {
  const seen = new Set();
  const out = [];
  for (const p of points || []) {
    const lon = Number(p.lon);
    const lat = Number(p.lat);
    if (!Number.isFinite(lon) || !Number.isFinite(lat)) continue;
    const key = `${lon.toFixed(6)},${lat.toFixed(6)}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ lon, lat });
  }
  return out;
}

function unwrapLons(points) {
  if (!points.length) return points;
  const origin = points[0].lon;
  return points.map((p) => {
    let lon = p.lon;
    while (lon - origin > 180) lon -= 360;
    while (lon - origin < -180) lon += 360;
    return { lon, lat: p.lat };
  });
}

function wrapLon(lon) {
  let x = lon;
  while (x > 180) x -= 360;
  while (x < -180) x += 360;
  return x;
}

function convexHullLonLat(points) {
  const uniq = unwrapLons(uniqueLonLat(points));
  if (uniq.length < 3) return [];
  const originLon = uniq.reduce((s, p) => s + p.lon, 0) / uniq.length;
  const originLat = uniq.reduce((s, p) => s + p.lat, 0) / uniq.length;
  const lat0 = (originLat * Math.PI) / 180;
  const mPerDegLat = 111320;
  const mPerDegLon = 111320 * Math.max(Math.cos(lat0), 1e-6);
  const xy = uniq.map((p) => ({
    lon: p.lon,
    lat: p.lat,
    x: (p.lon - originLon) * mPerDegLon,
    y: (p.lat - originLat) * mPerDegLat,
  }));
  xy.sort((a, b) => a.x - b.x || a.y - b.y);
  const cross = (o, a, b) => (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
  const lower = [];
  const upper = [];
  for (const p of xy) {
    while (lower.length >= 2 && cross(lower[lower.length - 2], lower[lower.length - 1], p) <= 0) {
      lower.pop();
    }
    lower.push(p);
  }
  for (let i = xy.length - 1; i >= 0; i--) {
    const p = xy[i];
    while (upper.length >= 2 && cross(upper[upper.length - 2], upper[upper.length - 1], p) <= 0) {
      upper.pop();
    }
    upper.push(p);
  }
  lower.pop();
  upper.pop();
  return lower.concat(upper).map((p) => ({ lon: wrapLon(p.lon), lat: p.lat }));
}

function removeHullEntity(entity) {
  if (entity && viewer) viewer.entities.remove(entity);
}

function makeHullEntity(points, fillCss, lineCss, show) {
  const hull = convexHullLonLat(points);
  if (hull.length < 3 || !viewer) return null;
  const Cesium = window.Cesium;
  const degrees = [];
  for (const p of hull) {
    degrees.push(p.lon, p.lat);
  }
  const positions = Cesium.Cartesian3.fromDegreesArray(degrees);
  const closed = positions.concat(positions[0]);
  return viewer.entities.add({
    polygon: {
      hierarchy: new Cesium.PolygonHierarchy(positions),
      material: Cesium.Color.fromCssColorString(fillCss).withAlpha(0.18),
      height: 0,
      outline: false,
    },
    polyline: {
      positions: closed,
      width: 2.5,
      material: Cesium.Color.fromCssColorString(lineCss),
      clampToGround: false,
      arcType: Cesium.ArcType.GEODESIC,
    },
    show,
  });
}

function rebuildIipHull() {
  removeHullEntity(iipHullEntity);
  iipHullEntity = makeHullEntity(lastImpacts, "#ff8c42", "#ffb347", showIipHull);
}

function rebuildTerminateHull() {
  removeHullEntity(terminateHullEntity);
  terminateHullEntity = makeHullEntity(lastTerminatePts, "#c084fc", "#e9d5ff", showTerminateHull);
}

export function setTerminateHull(points) {
  lastTerminatePts = points || [];
  rebuildTerminateHull();
  requestRender();
}

export function setIipHullVisible(visible) {
  showIipHull = Boolean(visible);
  if (iipHullEntity) iipHullEntity.show = showIipHull;
  else if (showIipHull) rebuildIipHull();
  requestRender();
}

export function setTerminateHullVisible(visible) {
  showTerminateHull = Boolean(visible);
  if (terminateHullEntity) terminateHullEntity.show = showTerminateHull;
  else if (showTerminateHull) rebuildTerminateHull();
  requestRender();
}

export function setImpactPoints(impacts) {
  const Cesium = window.Cesium;
  if (!viewer) return;
  if (!impactPoints) {
    impactPoints = viewer.scene.primitives.add(new Cesium.PointPrimitiveCollection());
  }
  impactPoints.removeAll();
  lastImpacts = impacts || [];
  const many = lastImpacts.length > 400;
  for (const p of lastImpacts) {
    impactPoints.add({
      position: Cesium.Cartesian3.fromDegrees(p.lon, p.lat, Math.max(p.alt, 0)),
      color: Cesium.Color.fromCssColorString(p.color || "#ffcc66"),
      pixelSize: many ? 6 : 9,
      outlineColor: Cesium.Color.WHITE.withAlpha(0.85),
      outlineWidth: many ? 0 : 1,
      disableDepthTestDistance: Number.POSITIVE_INFINITY,
    });
  }
  rebuildIipHull();
  requestRender();
}

export function setImpactVisible(visible) {
  if (impactPoints) impactPoints.show = visible;
  requestRender();
}

export async function setKdeGrid(grid) {
  const Cesium = window.Cesium;
  clearKde();
  lastGrid = grid;
  if (!viewer || !grid || !grid.values?.length || !(grid.max_value > 0)) {
    requestRender();
    return;
  }
  const canvas = densityCanvas(grid);
  lastKdeCanvas = canvas;
  const rectangle = Cesium.Rectangle.fromDegrees(grid.west, grid.south, grid.east, grid.north);
  const material = Cesium.Material.fromType("Image", { image: canvas });
  kdePrim = viewer.scene.primitives.add(new Cesium.Primitive({
    geometryInstances: new Cesium.GeometryInstance({
      geometry: new Cesium.RectangleGeometry({
        rectangle,
        vertexFormat: Cesium.EllipsoidSurfaceAppearance.VERTEX_FORMAT,
        height: 0,
      }),
    }),
    appearance: new Cesium.EllipsoidSurfaceAppearance({
      aboveGround: false,
      material,
    }),
    asynchronous: false,
    allowPicking: false,
  }));
  kdePrim.show = true;
  requestRender();
}

export function setKdeVisible(visible) {
  if (kdePrim) kdePrim.show = visible;
  requestRender();
}

export function clearRiskOverlay() {
  lastImpacts = [];
  lastGrid = null;
  if (impactPoints) impactPoints.removeAll();
  clearKde();
  rebuildIipHull();
}

export function kdeBounds(grid) {
  if (!grid) return lastGrid;
  return {
    west: grid.west,
    south: grid.south,
    east: grid.east,
    north: grid.north,
  };
}

export function impactBounds() {
  const pts = lastImpacts.filter((p) => Number.isFinite(p.lon) && Number.isFinite(p.lat));
  if (!pts.length) return null;
  const bounds = {
    west: Math.min(...pts.map((p) => p.lon)),
    south: Math.min(...pts.map((p) => p.lat)),
    east: Math.max(...pts.map((p) => p.lon)),
    north: Math.max(...pts.map((p) => p.lat)),
  };
  if (bounds.east - bounds.west < 0.05) {
    bounds.west -= 0.25;
    bounds.east += 0.25;
  }
  if (bounds.north - bounds.south < 0.05) {
    bounds.south -= 0.25;
    bounds.north += 0.25;
  }
  return bounds;
}

function clearKde() {
  if (kdePrim && viewer) {
    viewer.scene.primitives.remove(kdePrim);
    kdePrim = null;
  }
  lastKdeCanvas = null;
}

function densityCanvas(grid) {
  const canvas = document.createElement("canvas");
  canvas.width = grid.nx;
  canvas.height = grid.ny;
  const ctx = canvas.getContext("2d");
  const img = ctx.createImageData(grid.nx, grid.ny);
  const max = Math.max(grid.max_value, 1e-18);
  for (let i = 0; i < grid.values.length; i++) {
    const t = Math.pow(Math.max(grid.values[i], 0) / max, 0.55);
    const o = i * 4;
    if (t < 0.008) {
      img.data[o + 3] = 0;
      continue;
    }
    const r = Math.min(255, Math.round(40 + 280 * t));
    const g = Math.min(255, Math.round(220 * Math.max(0, 1 - t) + 40 * t));
    const b = Math.min(255, Math.round(30 + 40 * (1 - t)));
    img.data[o] = r;
    img.data[o + 1] = g;
    img.data[o + 2] = b;
    img.data[o + 3] = Math.round(70 + 185 * t);
  }
  ctx.putImageData(img, 0, 0);
  return canvas;
}

function travelDirection(Cesium, positions) {
  if (!positions || positions.length < 2) return null;
  const start = positions[0];
  let ahead = positions[1];
  for (let i = 1; i < Math.min(positions.length, 16); i++) {
    if (Cesium.Cartesian3.distance(start, positions[i]) > 800) {
      ahead = positions[i];
      break;
    }
  }
  const delta = Cesium.Cartesian3.subtract(ahead, start, new Cesium.Cartesian3());
  if (Cesium.Cartesian3.magnitude(delta) < 10) return null;
  const dir = Cesium.Cartesian3.normalize(delta, new Cesium.Cartesian3());
  let trackM = 0;
  for (let i = 1; i < positions.length; i++) {
    trackM += Cesium.Cartesian3.distance(positions[i - 1], positions[i]);
  }
  const len = Math.min(36_000, Math.max(7_500, trackM * 0.055));
  return { start, dir, len };
}

function axisModelMatrix(Cesium, origin, dir, length, along0) {
  const center = Cesium.Cartesian3.add(
    origin,
    Cesium.Cartesian3.multiplyByScalar(dir, along0 + length * 0.5, new Cesium.Cartesian3()),
    new Cesium.Cartesian3(),
  );
  let right = Cesium.Cartesian3.cross(dir, origin, new Cesium.Cartesian3());
  if (Cesium.Cartesian3.magnitudeSquared(right) < 1e-12) {
    Cesium.Cartesian3.cross(dir, Cesium.Cartesian3.UNIT_Z, right);
  }
  Cesium.Cartesian3.normalize(right, right);
  const y = Cesium.Cartesian3.cross(dir, right, new Cesium.Cartesian3());
  Cesium.Cartesian3.normalize(y, y);
  const rot = new Cesium.Matrix3();
  Cesium.Matrix3.setColumn(rot, 0, right, rot);
  Cesium.Matrix3.setColumn(rot, 1, y, rot);
  Cesium.Matrix3.setColumn(rot, 2, Cesium.Cartesian3.clone(dir), rot);
  return Cesium.Matrix4.fromRotationTranslation(rot, center);
}

function addTravelArrow(Cesium, positions, color) {
  if (!arrowPrims) return null;
  const travel = travelDirection(Cesium, positions);
  if (!travel) return null;
  const { start, dir, len } = travel;
  const shaftLen = len * 0.55;
  const headLen = len * 0.45;
  const vertexFormat = Cesium.PerInstanceColorAppearance.VERTEX_FORMAT;
  const instances = [
    new Cesium.GeometryInstance({
      id: "shaft",
      geometry: new Cesium.CylinderGeometry({
        length: shaftLen,
        topRadius: len * 0.055,
        bottomRadius: len * 0.055,
        slices: 8,
        vertexFormat,
      }),
      modelMatrix: axisModelMatrix(Cesium, start, dir, shaftLen, 0),
      attributes: {
        color: Cesium.ColorGeometryInstanceAttribute.fromColor(color),
      },
    }),
    new Cesium.GeometryInstance({
      id: "head",
      geometry: new Cesium.CylinderGeometry({
        length: headLen,
        topRadius: 0,
        bottomRadius: len * 0.22,
        slices: 8,
        vertexFormat,
      }),
      modelMatrix: axisModelMatrix(Cesium, start, dir, headLen, shaftLen),
      attributes: {
        color: Cesium.ColorGeometryInstanceAttribute.fromColor(color),
      },
    }),
  ];
  return arrowPrims.add(new Cesium.Primitive({
    geometryInstances: instances,
    appearance: new Cesium.PerInstanceColorAppearance({
      flat: false,
      translucent: color.alpha < 1,
      closed: true,
    }),
    asynchronous: false,
    allowPicking: false,
    releaseGeometryInstances: true,
  }));
}

function tintTravelArrow(prim, color) {
  if (!prim) return;
  const value = window.Cesium.ColorGeometryInstanceAttribute.toValue(color);
  for (const id of ["shaft", "head"]) {
    const attr = prim.getGeometryInstanceAttributes(id);
    if (attr) attr.color = value;
  }
}

function padRectangle(rectangle) {
  const Cesium = window.Cesium;
  const width = Math.max(Cesium.Rectangle.computeWidth(rectangle), 0.08);
  const height = Math.max(Cesium.Rectangle.computeHeight(rectangle), 0.08);
  return Cesium.Rectangle.fromRadians(
    rectangle.west - width * 0.15,
    rectangle.south - height * 0.15,
    rectangle.east + width * 0.15,
    rectangle.north + height * 0.15,
  );
}
