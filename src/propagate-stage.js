/**
 * Spent-stage propagation from a state on an existing trajectory.
 *
 * The mode field is prefilled (often "Nominal") before the user types.
 * That visible name is the mode. A blank stored draft must not hide it.
 *
 * Wind is not part of this request. The simulate command coasts with the
 * Environment wind already selected for the mission, the same wind other
 * generated trajectories use.
 */

export function propagationModeName(shown, stored) {
  const visible = typeof shown === "string" ? shown.trim() : "";
  if (visible) return visible;
  const saved = typeof stored === "string" ? stored.trim() : "";
  return saved;
}

export async function propagateStage(input) {
  const modeName = propagationModeName(input.shownMode, input.storedMode);
  if (!input.trackId) {
    return { ok: false, tracks: [], message: "Pick a source trajectory first." };
  }
  if (!input.objectId || !modeName) {
    return {
      ok: false,
      tracks: [],
      message: "Enter a mode name, then propagate the spent stage onto this object.",
    };
  }
  const stage = input.stage || {};
  const dist = stage.dist || "point";
  let timeS = input.timeS;
  if (dist === "uniform" && stage.tMin != null && stage.tMax != null
      && Number.isFinite(Number(stage.tMin)) && Number.isFinite(Number(stage.tMax))) {
    timeS = (Number(stage.tMin) + Number(stage.tMax)) / 2;
  }
  const sample = input.sample && input.sample.track_id === input.trackId ? input.sample : null;
  const sliderTime = Number(input.sliderTime);
  const time = sample?.time_s ?? timeS ?? (Number.isFinite(sliderTime) ? sliderTime : null);
  if (time == null || Number.isNaN(Number(time))) {
    return { ok: false, tracks: [], message: "Pick a separation time on the source trajectory." };
  }
  const spec = {
    source_track_id: input.trackId,
    time_s: Number(time),
    ballistic_coeff: Number(stage.ballistic_coeff),
    object_id: input.objectId,
    object_name: null,
    mode_name: modeName,
    dist,
    count: dist === "point" ? 1 : Math.max(1, Number(stage.count) || 1),
    t_min: dist === "uniform" ? Number(stage.tMin) : null,
    t_max: dist === "uniform" ? Number(stage.tMax) : null,
    sigma_s: dist === "normal" ? Number(stage.sigma) : null,
    n_sigma: dist === "normal" ? (Number(input.nSigma) || 3) : 3,
    seed: Number(stage.seed) || 1,
  };
  const result = await input.invoke("simulate_spent_stage", { spec });
  const tracks = result?.tracks || [];
  const label = `Spent stage · ${tracks.length} traj`;
  return {
    ok: true,
    tracks,
    result,
    modeName,
    message: tracks.length ? label : `${label} — no tracks returned`,
  };
}
