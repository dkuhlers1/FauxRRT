/**
 * Globe layer rows. A row is drawn only when that layer has something on the
 * globe, and it goes away when the last item does.
 */

export const GLOBE_LAYER_ROWS = [
  { id: "trajectories", label: "Trajectories", inputId: "show-trajectories" },
  { id: "wind", label: "Wind", inputId: "show-wind" },
  { id: "impacts", label: "Impacts", inputId: "show-impacts" },
  { id: "kde", label: "KDE", inputId: "show-kde" },
  { id: "vessel", label: "Vessel risk", inputId: "show-vessel-risk" },
  { id: "isolines", label: "Risk isolines", inputId: "show-risk-isolines" },
  { id: "boats", label: "Boats", inputId: "show-boats" },
  { id: "iip", label: "IIP boundary", inputId: "show-iip-boundary" },
  { id: "terminate", label: "Terminate boundary", inputId: "show-terminate-boundary" },
];

export function trackDrawsOnGlobe(track) {
  if (!track || track.show_path === false || track.visible === false) return false;
  return (track.display_lla?.length || 0) >= 6;
}

/** Streaks exist for a constant vector, a loaded surface grid, or a historical field with samples. */
export function windFieldOnGlobe(field) {
  if (!field || field.kind === "off" || field.type === "off") return false;
  if (field.kind === "historical" || field.type === "historical") {
    return Boolean(!field.loading && !field.error && field.regions?.length);
  }
  return field.kind === "constant" || field.kind === "grid"
    || field.type === "constant" || field.type === "surface";
}

export function kdeGridOnGlobe(grid) {
  return Boolean(grid && grid.values?.length && Number(grid.max_value) > 0);
}

export function vesselCellOnGlobe(cell) {
  return (cell?.boundary || []).length >= 6;
}

export function isolineOnGlobe(line) {
  return (line?.line || []).length >= 4;
}

export function boatOnGlobe(boat) {
  if (!boat || boat.visible === false) return false;
  return Number.isFinite(Number(boat.lon)) && Number.isFinite(Number(boat.lat));
}

function countOf(value) {
  const n = Number(value);
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/**
 * @param {{
 *   trajectories?: number,
 *   wind?: boolean,
 *   impacts?: number,
 *   kde?: boolean,
 *   vessel?: number,
 *   isolines?: number,
 *   boats?: number,
 *   terminate?: number,
 * }} counts
 */
export function layerPresenceFromCounts(counts) {
  const c = counts || {};
  const impacts = countOf(c.impacts);
  const terminate = countOf(c.terminate);
  return {
    trajectories: countOf(c.trajectories) > 0,
    wind: Boolean(c.wind),
    impacts: impacts > 0,
    kde: Boolean(c.kde),
    vessel: countOf(c.vessel) > 0,
    isolines: countOf(c.isolines) > 0,
    boats: countOf(c.boats) > 0,
    // The hull is a polygon, so it needs three or more positions.
    iip: impacts >= 3,
    terminate: terminate >= 3,
  };
}

export function globeLayersToShow(presence) {
  const flags = presence || {};
  return GLOBE_LAYER_ROWS.filter((row) => Boolean(flags[row.id]));
}
