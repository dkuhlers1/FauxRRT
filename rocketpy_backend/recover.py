#!/usr/bin/env python3
"""Recover thrust and aerodynamic products from one trajectory.

The fitter never calls RocketPy. It reads a state table and solves a linear
force residual. Dynamic pressure uses the ISO 2533 standard atmosphere, the
same model RocketPy selects with ``standard_atmosphere``.

State table
-----------
One row per sample. Velocity and acceleration are in the local East, North, Up
frame used by a RocketPy flight (x east, y north, z up). Acceleration is the
kinematic acceleration, so it still contains gravity.

Required columns:

- time_s
- east_m, north_m, up_m
  ``up_m`` is geometric altitude above sea level.
- yaw_rad, pitch_rad, roll_rad
- mass_kg

Velocity (``vel_east_mps``, ``vel_north_mps``, ``vel_up_mps``) and acceleration
(``acc_east_mps2``, ``acc_north_mps2``, ``acc_up_mps2``) are used when the file
has them. If velocity is missing it is the derivative of this East-North-Up
position with respect to time. If acceleration is missing it is the derivative
of velocity — the file's velocity, or the velocity just derived from position.
Differences follow ``numpy.gradient`` on the actual sample times, including
uneven spacing: second-order central differences in the interior and one-sided
differences at the ends. The samples are not smoothed. Latitude and longitude
are not differenced; a lat/lon track is converted to these metres first.

Nose direction (body +Z, tail to nose), in that same East-North-Up frame:

- pitch_rad: elevation of the nose above the local horizontal
- yaw_rad: heading of the nose, radians from north toward east
- roll_rad: roll about the nose. The column is required. An axisymmetric fit
  does not use roll to point the nose.

    east  = cos(pitch) * sin(yaw)
    north = cos(pitch) * cos(yaw)
    up    = sin(pitch)

Optional ``# latitude_deg=`` and ``# longitude_deg=`` comments (or the same
JSON fields) set the launch site used for gravity and Earth rotation.

The normal force per unit dynamic pressure is the Allen–Perkins split

    C_α · sin(α) · cos(α) + C_cross · sin²(α)

``C_α`` is the fitted Cl_α·A. ``C_cross`` is the broadside drag area (m²), the
cylinder extrapolation. It is fit only when some sample has |α| large enough
that sin²α separates from the linear term, about 15° or more. A flight that
stays at small α leaves ``C_cross`` unset. Optional ``length_m`` and
``diameter_m`` may supply an assumed 1.2·L·D; that value is not a fit.
"""

from __future__ import annotations

import json
import math
import sys

import numpy as np

REQUIRED_GROUPS = {
    "time": ("time_s",),
    "position": ("east_m", "north_m", "up_m"),
    "velocity": ("vel_east_mps", "vel_north_mps", "vel_up_mps"),
    "acceleration": ("acc_east_mps2", "acc_north_mps2", "acc_up_mps2"),
    "orientation": ("yaw_rad", "pitch_rad", "roll_rad"),
    "mass": ("mass_kg",),
}

# Named in the clear error when absent. The fit does not invent them.
# Velocity and acceleration are differentiated from position when those columns are absent.
STATED_GROUPS = ("orientation", "mass")
REQUIRED_TO_LOAD = ("time", "position", *STATED_GROUPS)

PRODUCT_NOTE = (
    "Drag and lift are a coefficient times area because the states cannot separate reference area."
)
LIFT_NOT_OBSERVABLE = (
    "Lift is not observable: the angle of attack stays near zero, so a lift-curve slope was not fit."
)
CROSS_NOT_OBSERVABLE = (
    "Broadside drag area is not observable: the angle of attack stays small, "
    "so the cylinder extrapolation was not fit."
)
CROSS_FIT_NOTE = (
    "Cylinder extrapolation of broadside drag area. "
    "Normal force per unit dynamic pressure is C_α·sin(α)·cos(α) + C_cross·sin²(α). "
    "At 90° only the second term remains."
)
ASSUMED_CROSS_NOTE = (
    "Assumed from 1.2·L·D for a circular cylinder in subsonic flow. Not fit from the states."
)

# ISO 2533 / RocketPy standard_atmosphere constants.
_EARTH_RADIUS_M = 6.3781e6
_AIR_R = 287.05287
_G0 = 9.80665
_ISA_H = np.array([-2e3, 0.0, 11e3, 20e3, 32e3, 47e3, 51e3, 71e3, 80e3])
_ISA_T = np.array([301.15, 288.15, 216.65, 216.65, 228.65, 270.65, 270.65, 214.65, 196.65])
_ISA_BETA = np.array([-6.5e-3, -6.5e-3, 0.0, 1e-3, 2.8e-3, 0.0, -2.8e-3, -2e-3, 0.0])
_ISA_P = np.array([1.27774e5, 1.01325e5, 2.26320e4, 5.47487e3, 8.680164e2, 1.10906e2, 6.69384e1, 3.95639e0, 8.86272e-2])
_ISA_GEOMETRIC = _EARTH_RADIUS_M * _ISA_H / (_EARTH_RADIUS_M - _ISA_H)

_Q_EXO_PA = 10.0
_Q_FIT_PA = 40.0
_ALPHA_MIN_RAD = math.radians(2.0)
_ALPHA_MAX_RAD = math.radians(28.0)
_CROSS_ALPHA_RAD = math.radians(15.0)
_MIN_SPEED = 20.0


def missing_message(groups: list[str]) -> str:
    """Error used when a required state was not in the file. Nothing is guessed."""
    ordered = [name for name in ("acceleration", "orientation", "mass", "time", "position", "velocity") if name in groups]
    if not ordered:
        ordered = list(groups)
    if len(ordered) == 1:
        listed = f"{ordered[0]} is"
    elif len(ordered) == 2:
        listed = f"{ordered[0]} and {ordered[1]} are"
    else:
        listed = ", ".join(ordered[:-1]) + f", and {ordered[-1]} are"
    return (
        f"Cannot recover missile parameters: {listed} missing. "
        "Time, position, velocity, acceleration, orientation, and mass are required. "
        "Nothing was guessed."
    )


def isa_temperature(height_m: np.ndarray) -> np.ndarray:
    h = np.asarray(height_m, dtype=float)
    return np.interp(h, _ISA_GEOMETRIC, _ISA_T, left=_ISA_T[0], right=_ISA_T[-1])


def isa_pressure(height_m: np.ndarray) -> np.ndarray:
    """ISO 2533 pressure, Pa, at geometric height above sea level."""
    h = np.asarray(height_m, dtype=float)
    geopotential = _EARTH_RADIUS_M * h / (_EARTH_RADIUS_M + h)
    out = np.empty(geopotential.shape, dtype=float)
    flat = geopotential.ravel()
    dest = out.ravel()
    for i, height in enumerate(flat):
        if height < _ISA_H[0]:
            dest[i] = _ISA_P[0]
            continue
        if height > _ISA_H[-1]:
            dest[i] = _ISA_P[-1]
            continue
        layer = int(np.searchsorted(_ISA_H, height, side="right") - 1)
        layer = min(max(layer, 0), len(_ISA_H) - 2)
        base_h = _ISA_H[layer]
        base_t = _ISA_T[layer]
        base_p = _ISA_P[layer]
        beta = _ISA_BETA[layer]
        if beta != 0.0:
            dest[i] = base_p * (1.0 + (beta / base_t) * (height - base_h)) ** (-_G0 / (beta * _AIR_R))
        else:
            temperature = base_t
            dest[i] = base_p * math.exp(-(height - base_h) * (_G0 / (_AIR_R * temperature)))
    return out


def isa_density(height_m: np.ndarray) -> np.ndarray:
    temperature = np.maximum(isa_temperature(height_m), 1.0)
    return isa_pressure(height_m) / (_AIR_R * temperature)


def somigliana_gravity(latitude_deg: float, height_m: np.ndarray) -> np.ndarray:
    """WGS84 Somigliana gravity, m/s², positive downward. Matches RocketPy's formula."""
    a = 6378137.0
    flattening = 1.0 / 298.257223563
    m_rot = 3.449786506841e-3
    g_equator = 9.7803253359
    k_somigliana = 1.931852652458e-3
    eccentricity_sq = 6.694379990141e-3
    sin_lat_sq = math.sin(math.radians(latitude_deg)) ** 2
    gravity = g_equator * ((1.0 + k_somigliana * sin_lat_sq) / math.sqrt(1.0 - eccentricity_sq * sin_lat_sq))
    height = np.asarray(height_m, dtype=float)
    correction = (
        1.0
        - height * 2.0 / a * (1.0 + flattening + m_rot - 2.0 * flattening * sin_lat_sq)
        + 3.0 * height**2 / a**2
    )
    return correction * gravity


def _header_value(text: str, key: str) -> float | None:
    prefix = f"# {key}="
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.lower().startswith(prefix.lower()):
            try:
                return float(stripped.split("=", 1)[1])
            except ValueError:
                return None
    return None


def _split_header(text: str) -> tuple[dict[str, float], str]:
    meta = {}
    for key in ("latitude_deg", "longitude_deg", "length_m", "diameter_m"):
        value = _header_value(text, key)
        if value is not None:
            meta[key] = value
    body = "\n".join(line for line in text.splitlines() if not line.strip().startswith("#"))
    return meta, body


def load_csv(text: str) -> dict:
    """Read the state table. Missing orientation or mass is an error.

    Velocity and acceleration may be omitted. ``recover_states`` fills them
    from the East-North-Up position.
    """
    meta, body = _split_header(text)
    lines = [line for line in body.splitlines() if line.strip()]
    if not lines:
        raise ValueError(missing_message(list(REQUIRED_TO_LOAD)))
    header = [cell.strip().lower() for cell in lines[0].split(",")]
    present = set(header)
    missing = [
        group
        for group in REQUIRED_TO_LOAD
        if any(column not in present for column in REQUIRED_GROUPS[group])
    ]
    if missing:
        raise ValueError(missing_message(missing))
    index = {name: header.index(name) for name in header}
    rows = []
    for line in lines[1:]:
        cells = [cell.strip() for cell in line.split(",")]
        if len(cells) < len(header):
            continue
        try:
            rows.append([float(cells[index[name]]) for name in header])
        except ValueError:
            continue
    if len(rows) < 4:
        raise ValueError("The trajectory needs at least four state samples to recover parameters.")
    data = {name: np.asarray([row[index[name]] for row in rows], dtype=float) for name in header}
    data.update(meta)
    return data


def load_table(payload: dict) -> dict:
    """Read the same columns from a JSON object."""
    missing = []
    arrays = {}
    for group in REQUIRED_TO_LOAD:
        columns = REQUIRED_GROUPS[group]
        if any(column not in payload or payload[column] is None for column in columns):
            missing.append(group)
            continue
        for column in columns:
            arrays[column] = np.asarray(payload[column], dtype=float).ravel()
    for group in ("velocity", "acceleration"):
        columns = REQUIRED_GROUPS[group]
        if any(column not in payload or payload[column] is None for column in columns):
            continue
        for column in columns:
            arrays[column] = np.asarray(payload[column], dtype=float).ravel()
    if missing:
        raise ValueError(missing_message(missing))
    n = len(arrays["time_s"])
    for name, values in arrays.items():
        if len(values) != n:
            raise ValueError(f"{name} must have one value per sample.")
    if n < 4:
        raise ValueError("The trajectory needs at least four state samples to recover parameters.")
    for key in ("latitude_deg", "longitude_deg", "length_m", "diameter_m"):
        if payload.get(key) is not None:
            arrays[key] = float(payload[key])
    return arrays


def _assumed_cross_area(table: dict) -> float | None:
    """1.2·L·D when both are supplied. Never used as a substitute for a fit."""
    length = table.get("length_m")
    diameter = table.get("diameter_m")
    if length is None or diameter is None:
        return None
    length = float(length)
    diameter = float(diameter)
    if not (math.isfinite(length) and math.isfinite(diameter) and length > 0.0 and diameter > 0.0):
        return None
    return 1.2 * length * diameter


def _cylinder_cross(
    dynamic_pressure: np.ndarray,
    alpha: np.ndarray,
    speed: np.ndarray,
    observed_normal: np.ndarray,
) -> tuple[float | None, bool]:
    """Broadside area from N/q = C_α sinα cosα + C_cross sin²α.

    ``C_α`` is a nuisance parameter here. The returned area is unset when no
    usable sample reaches about 15°, or when sin²α lies on the linear term.
    """
    usable = (speed > _MIN_SPEED) & (dynamic_pressure >= _Q_FIT_PA) & (alpha <= math.radians(92.0))
    if int(np.count_nonzero(usable & (alpha >= _CROSS_ALPHA_RAD))) < 1:
        return None, False
    fit = usable & (alpha >= math.radians(1.0))
    if int(np.count_nonzero(fit)) < 4:
        return None, False
    sine = np.sin(alpha[fit])
    cosine = np.cos(alpha[fit])
    pressure = dynamic_pressure[fit]
    linear = pressure * sine * cosine
    broadside = pressure * sine**2
    observed = observed_normal[fit]
    linear_norm = float(np.linalg.norm(linear))
    broadside_norm = float(np.linalg.norm(broadside))
    if broadside_norm <= 1e-8:
        return None, False
    # Near 90° the linear term drops out and the broadside area stands alone.
    if linear_norm <= 1e-6 * broadside_norm:
        value = float(np.dot(observed, broadside) / np.dot(broadside, broadside))
        return max(value, 0.0), True
    correlation = abs(float(np.dot(linear, broadside))) / (linear_norm * broadside_norm)
    if correlation > 0.995:
        return None, False
    coefficient, _, rank, _ = np.linalg.lstsq(
        np.column_stack([linear, broadside]),
        observed,
        rcond=None,
    )
    if int(rank) < 2:
        return None, False
    return max(float(coefficient[1]), 0.0), True


def _body_axis(yaw: np.ndarray, pitch: np.ndarray) -> np.ndarray:
    cos_pitch = np.cos(pitch)
    return np.column_stack([cos_pitch * np.sin(yaw), cos_pitch * np.cos(yaw), np.sin(pitch)])


def _piecewise_segments(time: np.ndarray, boosting: np.ndarray, endo: np.ndarray) -> list[dict]:
    if len(time) == 0:
        return []
    segments = []
    start = 0
    for i in range(1, len(time) + 1):
        if i == len(time) or boosting[i] != boosting[start] or endo[i] != endo[start]:
            segments.append(
                {
                    "t_start_s": float(time[start]),
                    "t_end_s": float(time[i - 1]),
                    "boosting": bool(boosting[start]),
                    "atmosphere": "endoatmospheric" if endo[start] else "exoatmospheric",
                }
            )
            start = i
    return segments


def _cd_a_from_curvature(time: np.ndarray, axial: np.ndarray, dynamic_pressure: np.ndarray) -> float | None:
    """Pick Cd*A so the remaining axial force is a smooth thrust-versus-time table.

    Thrust and a constant Cd*A are collinear in the axial force. Dynamic pressure
    during a burn is curved, while a thrust curve is piecewise smooth, so the
    Cd*A that minimizes the trimmed second difference of thrust is the drag product.
    """
    if len(time) < 8 or float(time[-1] - time[0]) < 0.3:
        return None
    if np.count_nonzero(dynamic_pressure > _Q_FIT_PA) < 5:
        return None
    grid = np.linspace(float(time[0]), float(time[-1]), max(48, len(time)))
    axial_grid = np.interp(grid, time, axial)
    pressure_grid = np.interp(grid, time, dynamic_pressure)
    ratio = np.abs(axial_grid) / np.maximum(pressure_grid, 1.0)
    upper = max(float(np.percentile(ratio, 95)) * 4.0, 0.05)
    candidates = np.linspace(0.0, upper, 320)

    def score(cd_a: float) -> float:
        thrust = axial_grid + pressure_grid * cd_a
        second = np.diff(thrust, n=2)
        magnitude = np.abs(second)
        limit = np.quantile(magnitude, 0.80)
        kept = second[magnitude <= limit]
        if kept.size < 6:
            kept = second
        return float(np.mean(kept**2))

    scores = np.array([score(value) for value in candidates])
    best_index = int(np.argmin(scores))
    lo = candidates[max(0, best_index - 1)]
    hi = candidates[min(len(candidates) - 1, best_index + 1)]
    fine = np.linspace(lo, hi, 41)
    fine_scores = np.array([score(value) for value in fine])
    best = float(fine[int(np.argmin(fine_scores))])
    span = float(scores.max() - scores.min())
    baseline = float(np.median(scores))
    if span < 1e-8 or (baseline > 0 and span / baseline < 0.01):
        return None
    return best


def _downsample(time: np.ndarray, values: np.ndarray, limit: int = 24) -> tuple[list[float], list[float]]:
    if len(time) == 0:
        return [], []
    if len(time) <= limit:
        chosen = np.arange(len(time))
    else:
        chosen = np.unique(np.round(np.linspace(0, len(time) - 1, limit)).astype(int))
    return [float(time[i]) for i in chosen], [float(values[i]) for i in chosen]


def _sample_columns(table: dict) -> dict[str, np.ndarray]:
    """One-dimensional per-sample columns, sorted by time."""
    time = np.asarray(table["time_s"], dtype=float).ravel()
    n = len(time)
    order = np.argsort(time)
    columns = {"time_s": time[order]}
    for key, value in table.items():
        if key == "time_s" or value is None or isinstance(value, (bool, int, float, np.floating, np.integer)):
            continue
        array = np.asarray(value, dtype=float).ravel()
        if array.shape[0] == n:
            columns[key] = array[order]
    return columns


def _gradient(values: np.ndarray, time: np.ndarray) -> np.ndarray:
    """``numpy.gradient`` on the sample times.

    Central differences in the interior, one-sided at the ends. ``edge_order=2``
    is the second-order one-sided stencil, so a constant acceleration is recovered
    on the whole series. The samples are not smoothed.
    """
    if len(time) < 2 or np.any(np.diff(time) <= 0):
        raise ValueError("Time stamps must be strictly increasing to derive velocity or acceleration.")
    return np.gradient(values, time, axis=0, edge_order=2)


def derived_kinematics_note(channels: list[str]) -> str | None:
    """One sentence naming the channels that were not in the file."""
    if channels == ["velocity", "acceleration"]:
        return "Velocity and acceleration were derived from position."
    if channels == ["velocity"]:
        return "Velocity was derived from position."
    if channels == ["acceleration"]:
        return "Acceleration was derived from velocity."
    return None


def with_derived_kinematics(table: dict) -> tuple[dict, list[str]]:
    """Sort by time and fill velocity or acceleration that the table left out.

    Position is the East-North-Up metres the fitter already uses. Velocity is
    dr/dt of that position. Acceleration is dv/dt. A velocity that is already
    present is kept; acceleration is then derived from that velocity.
    """
    columns = _sample_columns(table)
    time = columns["time_s"]
    position = np.column_stack([columns["east_m"], columns["north_m"], columns["up_m"]])
    derived: list[str] = []
    if all(name in columns for name in REQUIRED_GROUPS["velocity"]):
        velocity = np.column_stack([columns[name] for name in REQUIRED_GROUPS["velocity"]])
    else:
        velocity = _gradient(position, time)
        for index, name in enumerate(REQUIRED_GROUPS["velocity"]):
            columns[name] = velocity[:, index]
        derived.append("velocity")
    if not all(name in columns for name in REQUIRED_GROUPS["acceleration"]):
        acceleration = _gradient(velocity, time)
        for index, name in enumerate(REQUIRED_GROUPS["acceleration"]):
            columns[name] = acceleration[:, index]
        derived.append("acceleration")
    out = dict(table)
    out.update(columns)
    return out, derived


def recover_states(table: dict) -> dict:
    """Fit thrust(t), Cd*A, and Cl_alpha*A. Does not call RocketPy."""
    table, derived = with_derived_kinematics(table)
    time = np.asarray(table["time_s"], dtype=float)
    yaw = np.asarray(table["yaw_rad"], dtype=float)
    pitch = np.asarray(table["pitch_rad"], dtype=float)
    mass = np.asarray(table["mass_kg"], dtype=float)
    altitude = np.asarray(table["up_m"], dtype=float)
    velocity = np.column_stack(
        [
            np.asarray(table["vel_east_mps"], dtype=float),
            np.asarray(table["vel_north_mps"], dtype=float),
            np.asarray(table["vel_up_mps"], dtype=float),
        ]
    )
    acceleration = np.column_stack(
        [
            np.asarray(table["acc_east_mps2"], dtype=float),
            np.asarray(table["acc_north_mps2"], dtype=float),
            np.asarray(table["acc_up_mps2"], dtype=float),
        ]
    )
    latitude = float(table.get("latitude_deg") or 0.0)
    if not np.isfinite(latitude):
        latitude = 0.0

    body = _body_axis(yaw, pitch)
    body_norm = np.linalg.norm(body, axis=1, keepdims=True)
    body = body / np.maximum(body_norm, 1e-12)
    speed = np.linalg.norm(velocity, axis=1)
    density = isa_density(altitude)
    dynamic_pressure = 0.5 * density * speed**2
    gravity = somigliana_gravity(latitude, altitude)
    earth_rate = 2.0 * math.pi / 86164.1
    latitude_rad = math.radians(latitude)
    omega_north = earth_rate * math.cos(latitude_rad)
    omega_up = earth_rate * math.sin(latitude_rad)
    east, north, up = velocity[:, 0], velocity[:, 1], velocity[:, 2]
    # Undo the Coriolis term RocketPy subtracts, then remove gravity.
    specific = np.column_stack(
        [
            acceleration[:, 0] + 2.0 * (up * omega_north - north * omega_up),
            acceleration[:, 1] + 2.0 * (east * omega_up),
            acceleration[:, 2] + gravity + 2.0 * (-east * omega_north),
        ]
    )
    force = specific * mass[:, None]

    velocity_hat = velocity / np.maximum(speed, 1e-9)[:, None]
    cosine = np.clip(np.sum(velocity_hat * body, axis=1), -1.0, 1.0)
    alpha = np.arccos(cosine)
    perpendicular = velocity_hat - cosine[:, None] * body
    perpendicular_norm = np.linalg.norm(perpendicular, axis=1)
    normal = np.divide(
        perpendicular,
        perpendicular_norm[:, None],
        out=np.zeros_like(perpendicular),
        where=perpendicular_norm[:, None] > 1e-8,
    )
    sine = np.sin(alpha)
    cosine_alpha = np.cos(alpha)

    mass_rate = np.gradient(mass, time)
    drop = -mass_rate
    peak_drop = float(np.percentile(np.maximum(drop, 0.0), 95)) if len(drop) else 0.0
    boost_threshold = max(0.02, 0.20 * peak_drop) if peak_drop > 0.02 else 0.02
    boosting = drop > boost_threshold
    endo = dynamic_pressure >= _Q_EXO_PA
    phases = _piecewise_segments(time, boosting, endo)

    lift_samples = (
        (speed > _MIN_SPEED)
        & endo
        & (alpha >= _ALPHA_MIN_RAD)
        & (alpha <= _ALPHA_MAX_RAD)
        & (dynamic_pressure >= _Q_FIT_PA)
    )
    lift_observable = int(np.count_nonzero(lift_samples)) >= 8
    cl_alpha_a = None
    lift_force = np.zeros_like(force)
    if lift_observable:
        # Perpendicular residual does not contain thrust or axial drag.
        perpendicular_force = force - np.sum(force * body, axis=1)[:, None] * body
        observed = np.sum(perpendicular_force * (-normal), axis=1)
        scale = dynamic_pressure * alpha * cosine_alpha
        denominator = float(np.sum(scale[lift_samples] ** 2))
        if denominator <= 0.0:
            lift_observable = False
        else:
            cl_alpha_a = float(np.sum(observed[lift_samples] * scale[lift_samples]) / denominator)
            if cl_alpha_a < 0.0:
                cl_alpha_a = 0.0
            lift_force = (
                -(dynamic_pressure * cl_alpha_a * alpha)[:, None]
                * (sine[:, None] * body + cosine_alpha[:, None] * normal)
            )

    axial = np.sum((force - lift_force) * body, axis=1)
    usable = (speed > _MIN_SPEED) & (alpha <= _ALPHA_MAX_RAD) & (cosine > 0.2)
    coast = usable & ~boosting & (dynamic_pressure >= _Q_FIT_PA)
    coast_observable = int(np.count_nonzero(coast)) >= 5
    cd_a_coast = None
    if coast_observable:
        weight = dynamic_pressure[coast]
        cd_a_coast = float(-np.sum(axial[coast] * weight) / np.sum(weight**2))
        if cd_a_coast < 0.0:
            cd_a_coast = 0.0

    boost = usable & boosting
    # Keep the interior of the burn so the burnout sample does not look like thrust.
    boost_index = np.flatnonzero(boost)
    if boost_index.size >= 10:
        trim = max(1, int(0.08 * boost_index.size))
        boost_fit = np.zeros(len(time), dtype=bool)
        boost_fit[boost_index[trim:-trim]] = True
    else:
        boost_fit = boost
    boost_fit &= dynamic_pressure >= _Q_FIT_PA
    cd_a_boost = None
    boost_observable = False
    if int(np.count_nonzero(boost_fit)) >= 8:
        cd_a_boost = _cd_a_from_curvature(time[boost_fit], axial[boost_fit], dynamic_pressure[boost_fit])
        boost_observable = cd_a_boost is not None

    thrust = np.zeros(len(time), dtype=float)
    if boost_observable:
        thrust[boosting] = axial[boosting] + dynamic_pressure[boosting] * cd_a_boost
        thrust = np.maximum(thrust, 0.0)
        peak = float(np.max(thrust)) if np.any(thrust > 0) else 0.0
        if peak > 0:
            thrust[thrust < 0.01 * peak] = 0.0
    burn = thrust > 0
    if np.any(burn):
        burn_time = time[burn]
        burn_start = float(burn_time[0])
        burn_end = float(burn_time[-1])
        peak_thrust = float(np.max(thrust))
        summary_t, summary_f = _downsample(time[burn], thrust[burn])
        thrust_time = [float(value) for value in time[burn]]
        thrust_n = [float(value) for value in thrust[burn]]
    else:
        burn_start = None
        burn_end = None
        peak_thrust = None
        summary_t, summary_f = [], []
        thrust_time, thrust_n = [], []

    perpendicular_force = force - np.sum(force * body, axis=1)[:, None] * body
    observed_normal = np.sum(perpendicular_force * (-normal), axis=1)
    c_cross, cross_observable = _cylinder_cross(dynamic_pressure, alpha, speed, observed_normal)
    assumed_cross = _assumed_cross_area(table)

    return {
        "ok": True,
        "phases": phases,
        "thrust": {
            "burn_start_s": burn_start,
            "burn_end_s": burn_end,
            "peak_n": peak_thrust,
            "time_s": thrust_time,
            "thrust_n": thrust_n,
            "summary_time_s": summary_t,
            "summary_thrust_n": summary_f,
            "observable": bool(burn_start is not None),
        },
        "cd_a_m2": {
            "boost": cd_a_boost,
            "coast": cd_a_coast,
            "boost_observable": boost_observable,
            "coast_observable": coast_observable,
        },
        "cl_alpha_a_m2_per_rad": cl_alpha_a if lift_observable else None,
        "lift_observable": lift_observable,
        "lift_note": None if lift_observable else LIFT_NOT_OBSERVABLE,
        "c_cross_m2": c_cross if cross_observable else None,
        "c_cross_observable": cross_observable,
        "c_cross_note": CROSS_FIT_NOTE if cross_observable else CROSS_NOT_OBSERVABLE,
        "c_cross_label": "cylinder extrapolation",
        "c_cross_assumed_m2": assumed_cross,
        "c_cross_assumed_note": None if assumed_cross is None else ASSUMED_CROSS_NOTE,
        "product_note": PRODUCT_NOTE,
        "derived_channels": derived,
        "derived_note": derived_kinematics_note(derived),
        "mass_kg": [float(value) for value in mass],
        "latitude_deg": latitude,
    }


def recover_csv(text: str) -> dict:
    return recover_states(load_csv(text))


def main() -> None:
    try:
        raw = sys.stdin.read()
        if not raw.strip():
            raise ValueError("Recovery input was empty.")
        stripped = raw.lstrip()
        if stripped.startswith("{"):
            payload = json.loads(raw)
            if not isinstance(payload, dict):
                raise ValueError("Recovery input must be a JSON object or a CSV state table.")
            result = recover_states(load_table(payload))
        else:
            result = recover_csv(raw)
        json.dump(result, sys.stdout, allow_nan=False)
        sys.stdout.write("\n")
    except Exception as exc:  # noqa: BLE001 - the CLI reports the message and a non-zero status
        json.dump({"ok": False, "error": str(exc)}, sys.stdout, allow_nan=False)
        sys.stdout.write("\n")
        sys.exit(1)


if __name__ == "__main__":
    main()
