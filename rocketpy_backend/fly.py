#!/usr/bin/env python3
"""FauxRRT 6DOF backend: JSON spec on stdin, trajectory JSON on stdout."""

from __future__ import annotations

import json
import math
import sys
import traceback


def _num(value, default=0.0):
    try:
        if value is None or value == "":
            return float(default)
        return float(value)
    except (TypeError, ValueError):
        return float(default)


def _arr3(value, default):
    if not isinstance(value, (list, tuple)) or len(value) < 3:
        return tuple(float(v) for v in default)
    return tuple(_num(value[i], default[i]) for i in range(3))


def _eval(fn, t):
    if fn is None:
        return None
    try:
        if callable(fn):
            v = fn(t)
        elif hasattr(fn, "get_value_opt"):
            v = fn.get_value_opt(t)
        elif hasattr(fn, "get_value"):
            v = fn.get_value(t)
        else:
            v = fn
        if hasattr(v, "__len__") and not isinstance(v, (str, bytes)):
            v = v[0]
        return float(v)
    except Exception:
        return None


def _times(flight):
    import numpy as np

    raw = getattr(flight, "time", None)
    if raw is None:
        raw = getattr(flight, "t", None)
    if raw is None:
        raise RuntimeError("RocketPy Flight has no time array")
    t = np.asarray(raw, dtype=float).ravel()
    if t.size < 2:
        raise RuntimeError("RocketPy Flight produced fewer than two states")
    max_n = 500
    if t.size > max_n:
        idx = np.linspace(0, t.size - 1, max_n).astype(int)
        idx[-1] = t.size - 1
        t = t[idx]
    return t


def _series(flight, name, times):
    fn = getattr(flight, name, None)
    out = []
    for t in times:
        v = _eval(fn, float(t))
        if v is None:
            raise RuntimeError(f"RocketPy Flight.{name} failed at t={t:.3f}s")
        out.append(v)
    return out


def _build_environment(spec):
    from rocketpy import Environment

    env_spec = spec.get("env") or {}
    lat = _num(env_spec.get("latitude"))
    lon = _num(env_spec.get("longitude"))
    elev = _num(env_spec.get("elevation_m"))
    env = Environment(latitude=lat, longitude=lon, elevation=elev)
    wind_speed = max(0.0, _num(env_spec.get("wind_speed_mps")))
    wind_from = _num(env_spec.get("wind_from_deg"))
    if wind_speed > 0:
        rad = math.radians(wind_from)
        east = -wind_speed * math.sin(rad)
        north = -wind_speed * math.cos(rad)
        env.set_atmospheric_model(type="custom_atmosphere", wind_u=east, wind_v=north)
    else:
        kind = str(env_spec.get("atmosphere") or "standard_atmosphere")
        try:
            env.set_atmospheric_model(type=kind)
        except Exception:
            env.set_atmospheric_model(type="standard_atmosphere")
    return env


def _build_motor(spec):
    from rocketpy import GenericMotor

    m = spec.get("motor") or {}
    burn = max(1e-3, _num(m.get("burn_time_s"), 3.9))
    thrust = max(0.0, _num(m.get("thrust_n"), 1500.0))
    thrust_source = [[0.0, thrust], [burn, thrust]]
    kwargs = dict(
        thrust_source=thrust_source,
        burn_time=burn,
        dry_mass=_num(m.get("dry_mass_kg"), 1.815),
        dry_inertia=_arr3(m.get("dry_inertia"), (0.125, 0.125, 0.002)),
        nozzle_radius=_num(m.get("nozzle_radius_m"), 0.033),
        chamber_radius=_num(m.get("chamber_radius_m"), 0.033),
        chamber_height=_num(m.get("chamber_height_m"), 0.6),
        chamber_position=_num(m.get("chamber_position_m"), 0.0),
        propellant_initial_mass=_num(m.get("propellant_mass_kg"), 2.5),
        nozzle_position=_num(m.get("nozzle_position_m"), 0.0),
    )
    try:
        return GenericMotor(**kwargs)
    except TypeError:
        kwargs.pop("chamber_position", None)
        kwargs.pop("nozzle_position", None)
        return GenericMotor(**kwargs)


def _build_rocket(spec, motor):
    from rocketpy import Rocket

    r = spec.get("rocket") or {}
    rocket = Rocket(
        radius=_num(r.get("radius_m"), 0.0635),
        mass=_num(r.get("mass_kg"), 14.426),
        inertia=_arr3(r.get("inertia"), (6.321, 6.321, 0.034)),
        power_off_drag=_num(r.get("power_off_cd"), 0.5),
        power_on_drag=_num(r.get("power_on_cd"), 0.5),
        center_of_mass_without_motor=_num(r.get("center_of_mass_m"), 0.0),
        coordinate_system_orientation="tail_to_nose",
    )
    rocket.add_motor(motor, position=_num(r.get("motor_position_m"), -1.255))
    upper = _num(r.get("rail_upper_m"), 0.0818)
    lower = _num(r.get("rail_lower_m"), -0.618)
    try:
        rocket.set_rail_buttons(upper_button_position=upper, lower_button_position=lower)
    except Exception:
        pass
    kind = str(r.get("nose_kind") or "von karman")
    rocket.add_nose(
        length=_num(r.get("nose_length_m"), 0.55829),
        kind=kind,
        position=_num(r.get("nose_position_m"), 1.278),
    )
    n_fins = int(_num(r.get("fin_n"), 4))
    rocket.add_trapezoidal_fins(
        n=max(3, min(8, n_fins)),
        root_chord=_num(r.get("fin_root_m"), 0.12),
        tip_chord=_num(r.get("fin_tip_m"), 0.06),
        span=_num(r.get("fin_span_m"), 0.11),
        position=_num(r.get("fin_position_m"), -1.04956),
    )
    tail_len = _num(r.get("tail_length_m"), 0.06)
    if tail_len > 1e-6:
        rocket.add_tail(
            top_radius=_num(r.get("tail_top_m"), 0.0635),
            bottom_radius=_num(r.get("tail_bottom_m"), 0.0435),
            length=tail_len,
            position=_num(r.get("tail_position_m"), -1.194656),
        )
    return rocket


def _build_flight(spec, rocket, env):
    from rocketpy import Flight

    f = spec.get("flight") or {}
    kwargs = dict(
        rocket=rocket,
        environment=env,
        rail_length=max(0.1, _num(f.get("rail_length_m"), 5.2)),
        inclination=_num(f.get("inclination_deg"), 85.0),
        heading=_num(f.get("heading_deg"), 0.0),
        max_time=max(1.0, _num(f.get("max_time_s"), 400.0)),
        terminate_on_apogee=False,
    )
    try:
        return Flight(**kwargs)
    except TypeError:
        kwargs.pop("max_time", None)
        kwargs.pop("terminate_on_apogee", None)
        return Flight(**kwargs)


def fly(spec):
    env = _build_environment(spec)
    motor = _build_motor(spec)
    rocket = _build_rocket(spec, motor)
    flight = _build_flight(spec, rocket, env)
    times = _times(flight)
    lat = _series(flight, "latitude", times)
    lon = _series(flight, "longitude", times)
    try:
        alt = _series(flight, "z", times)
    except Exception:
        alt = _series(flight, "altitude", times)
        elev = _num((spec.get("env") or {}).get("elevation_m"))
        if alt and abs(alt[0]) < 5.0 and elev > 5.0:
            alt = [a + elev for a in alt]
    apogee = None
    for name in ("apogee", "apogee_altitude"):
        v = getattr(flight, name, None)
        if v is None:
            continue
        try:
            apogee = float(v)
            break
        except (TypeError, ValueError):
            continue
    if apogee is None and alt:
        apogee = max(alt)
    impact_t = None
    for name in ("t_final", "t_impact", "impact_time"):
        v = getattr(flight, name, None)
        if v is None:
            continue
        try:
            impact_t = float(v)
            break
        except (TypeError, ValueError):
            continue
    if impact_t is None and len(times):
        impact_t = float(times[-1])
    return {
        "times": [float(t) for t in times],
        "lat": lat,
        "lon": lon,
        "alt": alt,
        "apogee_m": apogee,
        "impact_time_s": impact_t,
    }


def _rocketpy_version():
    try:
        from importlib.metadata import version

        return version("rocketpy")
    except Exception:
        import rocketpy

        return getattr(rocketpy, "__version__", "unknown")


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--check":
        try:
            import rocketpy  # noqa: F401

            json.dump({"ok": True, "version": _rocketpy_version()}, sys.stdout)
        except Exception as exc:
            json.dump({"ok": False, "error": str(exc)}, sys.stdout)
        sys.stdout.write("\n")
        return
    try:
        spec = json.load(sys.stdin)
        if not isinstance(spec, dict):
            raise ValueError("RocketPy spec must be a JSON object")
        result = fly(spec)
        json.dump({"ok": True, **result}, sys.stdout, allow_nan=False)
        sys.stdout.write("\n")
    except Exception as exc:
        traceback.print_exc(file=sys.stderr)
        json.dump({"ok": False, "error": str(exc)}, sys.stdout)
        sys.stdout.write("\n")


if __name__ == "__main__":
    main()
