/**
 * Parameter recovery for the selected trajectory in the Trajectories pop-out.
 *
 * The states determine Cd·A and Cl_alpha·A, not a coefficient and a reference
 * area separately. The panel says that, and it names any product the fit
 * could not observe.
 */

export function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[ch]));
}

function finite(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function fmt(value, digits = 4) {
  const number = finite(value);
  if (number == null) return "—";
  const abs = Math.abs(number);
  if (abs !== 0 && (abs < 0.001 || abs >= 100000)) return number.toExponential(2);
  return number.toFixed(digits);
}

function fmtTime(value) {
  const number = finite(value);
  return number == null ? "—" : `${number.toFixed(2)} s`;
}

function phaseLine(phase) {
  const motion = phase?.boosting ? "boosting" : "not boosting";
  const air = phase?.atmosphere === "exoatmospheric" ? "exoatmospheric" : "endoatmospheric";
  return `${fmtTime(phase?.t_start_s)}–${fmtTime(phase?.t_end_s)} · ${motion} · ${air}`;
}

function productValue(value, observable, unit) {
  if (!observable || finite(value) == null) return "not observable";
  return `${fmt(value)} ${unit}`;
}

/**
 * Markup for the selected trajectory's recovery action and last result.
 * `track` is `{ id, name }` or null. `result` is the recovery payload.
 */
export function recoveryPanelHtml({ track = null, result = null, error = "", busy = false } = {}) {
  const name = track?.name ? escapeHtml(track.name) : "none selected";
  const disabled = !track || busy ? " disabled" : "";
  const note = result?.product_note
    || "Drag and lift are a coefficient times area because the states cannot separate reference area.";
  const phases = Array.isArray(result?.phases) ? result.phases : [];
  const thrust = result?.thrust || null;
  const cd = result?.cd_a_m2 || null;
  const summaryTimes = thrust?.summary_time_s || [];
  const summaryThrust = thrust?.summary_thrust_n || [];
  const summary = summaryTimes
    .map((time, index) => `${fmtTime(time)}: ${fmt(summaryThrust[index], 0)} N`)
    .join("; ");
  const thrustBody = !result
    ? ""
    : thrust?.observable
      ? `<p data-role="thrust-summary">Peak ${fmt(thrust.peak_n, 0)} N from ${fmtTime(thrust.burn_start_s)} to ${fmtTime(thrust.burn_end_s)}.${summary ? ` ${escapeHtml(summary)}.` : ""}</p>`
      : `<p data-role="thrust-summary">Thrust: not observable</p>`;
  const phaseBody = phases.length
    ? `<ul class="recover-phases" data-role="phases">${phases.map((phase) => `<li>${escapeHtml(phaseLine(phase))}</li>`).join("")}</ul>`
    : "";
  const products = result
    ? `<dl class="recover-products">
        <dt>Cd·A boost</dt><dd data-role="cd-a-boost">${escapeHtml(productValue(cd?.boost, cd?.boost_observable, "m²"))}</dd>
        <dt>Cd·A coast</dt><dd data-role="cd-a-coast">${escapeHtml(productValue(cd?.coast, cd?.coast_observable, "m²"))}</dd>
        <dt>Cl_alpha·A</dt><dd data-role="cl-alpha-a">${escapeHtml(productValue(result.cl_alpha_a_m2_per_rad, result.lift_observable, "m²/rad"))}</dd>
        <dt>C_cross cylinder extrapolation</dt><dd data-role="c-cross">${escapeHtml(productValue(result.c_cross_m2, result.c_cross_observable, "m²"))}</dd>
        ${finite(result.c_cross_assumed_m2) != null ? `<dt>C_cross from 1.2·L·D</dt><dd data-role="c-cross-assumed">assumed ${escapeHtml(fmt(result.c_cross_assumed_m2))} m²</dd>` : ""}
      </dl>
      ${result.lift_observable ? "" : `<p class="recover-note" data-role="lift-note">${escapeHtml(result.lift_note || "Lift is not observable: the angle of attack stays near zero, so a lift-curve slope was not fit.")}</p>`}
      <p class="recover-note" data-role="c-cross-note">${escapeHtml(result.c_cross_note || (result.c_cross_observable ? "Cylinder extrapolation of broadside drag area." : "Broadside drag area is not observable: the angle of attack stays small, so the cylinder extrapolation was not fit."))}</p>
      ${finite(result.c_cross_assumed_m2) != null ? `<p class="recover-note" data-role="c-cross-assumed-note">${escapeHtml(result.c_cross_assumed_note || "Assumed from 1.2·L·D. Not fit from the states.")}</p>` : ""}`
    : "";
  const errorBody = error
    ? `<p class="schema-error" data-role="recover-error">${escapeHtml(error)}</p>`
    : "";
  const status = busy ? `<p class="muted" data-role="recover-status">Recovering parameters…</p>` : "";
  const derivedNote = result?.derived_note
    ? `<p class="recover-note" data-role="derived-note">${escapeHtml(result.derived_note)}</p>`
    : "";
  return `<section class="recover-panel" data-role="recover-panel" data-track-id="${track ? escapeHtml(track.id) : ""}">
    <h3>Recover parameters</h3>
    <p class="muted" data-role="selected-trajectory">Selected trajectory: ${name}</p>
    <button type="button" data-act="recover-parameters" class="compact"${disabled}>Recover parameters</button>
    ${status}
    ${errorBody}
    ${derivedNote}
    <p class="recover-note" data-role="product-note">${escapeHtml(note)}</p>
    ${phaseBody}
    ${thrustBody}
    ${products}
  </section>`;
}
