#!/usr/bin/env python3
"""Generate a RocketPy 6DOF boost + coast from Eglin AFB southeast toward Key West."""

from __future__ import annotations

import csv
import math
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "rocketpy_backend"))

from fly import _build_environment, _build_flight, _build_motor, _build_rocket, _eval  # noqa: E402

OUT = Path(__file__).resolve().parent / "eglin_keywest_6dof.csv"

EGLIN = (30.4832, -86.5254, 26.0)
KEY_WEST = (24.5551, -81.7826)
BURN_S = 20.0


def bearing_deg(lat1, lon1, lat2, lon2) -> float:
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dl = math.radians(lon2 - lon1)
    y = math.sin(dl) * math.cos(p2)
    x = math.cos(p1) * math.sin(p2) - math.sin(p1) * math.cos(p2) * math.cos(dl)
    return (math.degrees(math.atan2(y, x)) + 360.0) % 360.0


def haversine_km(lat1, lon1, lat2, lon2) -> float:
    r = 6371.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp = p2 - p1
    dl = math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


def spec(heading: float, inclination: float) -> dict:
    # Calisto-like 6DOF airframe (stable static margin) with a longer-burning motor
    # so the CSV has a clear boost, then a Gulf coast that stops short of Key West.
    return {
        "env": {
            "latitude": EGLIN[0],
            "longitude": EGLIN[1],
            "elevation_m": EGLIN[2],
            "atmosphere": "standard_atmosphere",
            "wind_speed_mps": 0.0,
            "wind_from_deg": 270.0,
        },
        "motor": {
            "thrust_n": 18000.0,
            "burn_time_s": BURN_S,
            "dry_mass_kg": 18.0,
            "propellant_mass_kg": 160.0,
            "dry_inertia": [0.4, 0.4, 0.008],
            "nozzle_radius_m": 0.045,
            "chamber_radius_m": 0.055,
            "chamber_height_m": 1.8,
            "nozzle_position_m": 0.0,
            "chamber_position_m": 0.35,
        },
        "rocket": {
            "radius_m": 0.1,
            "mass_kg": 80.0,
            "inertia": [40.0, 40.0, 0.2],
            "power_off_cd": 0.5,
            "power_on_cd": 0.5,
            "center_of_mass_m": 0.0,
            "motor_position_m": -1.255,
            "nose_length_m": 0.8,
            "nose_kind": "von karman",
            "nose_position_m": 1.278,
            "fin_n": 4,
            "fin_root_m": 0.20,
            "fin_tip_m": 0.09,
            "fin_span_m": 0.22,
            "fin_position_m": -1.04956,
            "tail_top_m": 0.1,
            "tail_bottom_m": 0.07,
            "tail_length_m": 0.06,
            "tail_position_m": -1.194656,
            "rail_upper_m": 0.0818,
            "rail_lower_m": -0.618,
        },
        "flight": {
            "rail_length_m": 12.0,
            "inclination_deg": inclination,
            "heading_deg": heading,
            "max_time_s": 500.0,
        },
    }


def series(flight, name, times):
    fn = getattr(flight, name, None)
    out = []
    for t in times:
        v = _eval(fn, float(t))
        if v is None:
            raise RuntimeError(f"Flight.{name} failed at t={t:.3f}s")
        out.append(v)
    return out


def sample_times(t_end: float, burn_s: float) -> list[float]:
    times = []
    t = 0.0
    # Dense boost + rail: keep the powered arc visible.
    while t < burn_s + 2.0 and t <= t_end:
        times.append(round(t, 4))
        t += 0.1
    t = max(times[-1] + 0.25, burn_s + 2.0) if times else 0.25
    while t < t_end:
        times.append(round(t, 4))
        t += 0.5 if t < burn_s + 40.0 else 1.0
    if not times or abs(times[-1] - t_end) > 0.05:
        times.append(round(t_end, 4))
    return times


def main() -> int:
    heading = bearing_deg(EGLIN[0], EGLIN[1], KEY_WEST[0], KEY_WEST[1])
    payload = spec(heading, 62.0)
    env = _build_environment(payload)
    motor = _build_motor(payload)
    rocket = _build_rocket(payload, motor)
    print("flying...", flush=True)
    flight = _build_flight(payload, rocket, env)
    print("flight done", flush=True)

    t_end = float(getattr(flight, "t_final", None) or getattr(flight, "t")[-1])
    times = sample_times(t_end, BURN_S)
    lat = series(flight, "latitude", times)
    lon = series(flight, "longitude", times)
    try:
        alt = series(flight, "z", times)
    except Exception:
        alt = series(flight, "altitude", times)
        if alt and abs(alt[0]) < 5.0:
            alt = [a + EGLIN[2] for a in alt]

    OUT.parent.mkdir(parents=True, exist_ok=True)
    with OUT.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["time", "lat", "lon", "alt"])
        for row in zip(times, lat, lon, alt):
            w.writerow([f"{row[0]:.4f}", f"{row[1]:.6f}", f"{row[2]:.6f}", f"{row[3]:.2f}"])

    apogee = max(alt)
    apo_t = times[alt.index(apogee)]
    n_boost = sum(1 for t in times if t <= BURN_S)
    down = haversine_km(EGLIN[0], EGLIN[1], lat[-1], lon[-1])
    remain = haversine_km(lat[-1], lon[-1], KEY_WEST[0], KEY_WEST[1])
    print(f"heading {heading:.1f} deg southeast (Eglin -> Key West)")
    print(f"wrote {OUT}")
    print(f"points {len(times)}  boost samples {n_boost}  t_burn {BURN_S:.0f}s  t_final {t_end:.1f}s")
    print(f"apogee {apogee:.0f} m at t={apo_t:.1f}s")
    print(f"impact {lat[-1]:.4f}, {lon[-1]:.4f}  alt {alt[-1]:.0f} m")
    print(f"downrange {down:.0f} km  remaining to Key West {remain:.0f} km")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
