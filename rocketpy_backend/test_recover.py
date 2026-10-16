#!/usr/bin/env python3
"""RocketPy round trip for trajectory parameter recovery.

Flies a known rocket, writes the state table the fitter reads, recovers
thrust and Cd*A (and Cl_alpha*A when alpha is large enough), then flies
RocketPy once more with those products.
"""

from __future__ import annotations

import math
import sys
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import recover  # noqa: E402

from rocketpy import Environment, Flight, GenericMotor, LinearGenericSurface, Rocket  # noqa: E402

LATITUDE = 32.99
LONGITUDE = -106.975
ELEVATION_M = 1400.0
RADIUS_M = 0.07
CL_ALPHA = 2.5
CD_ON = 0.40
CD_OFF = 0.62
AREA_M2 = math.pi * RADIUS_M**2
INCLINATION_DEG = 55.0
HEADING_DEG = 15.0


def _motor(thrust, burn_time):
    return GenericMotor(
        thrust_source=thrust,
        burn_time=burn_time,
        dry_mass=4.0,
        dry_inertia=(0.05, 0.05, 0.005),
        nozzle_radius=0.0,
        chamber_radius=0.04,
        chamber_height=0.3,
        chamber_position=0.0,
        propellant_initial_mass=3.0,
        center_of_dry_mass_position=0.0,
    )


def _lift_coefficient(value):
    def coefficient(alpha, beta, mach, reynolds, pitch_rate, yaw_rate, roll_rate):
        return value

    return coefficient


def _rocket(cd_on, cd_off, cl_alpha, thrust, burn_time):
    rocket = Rocket(
        radius=RADIUS_M,
        mass=12.0,
        inertia=(2.0, 2.0, 0.02),
        power_off_drag=cd_off,
        power_on_drag=cd_on,
        center_of_mass_without_motor=0.0,
        coordinate_system_orientation="tail_to_nose",
    )
    rocket.add_motor(_motor(thrust, burn_time), position=0.0)
    if cl_alpha:
        surface = LinearGenericSurface(
            reference_area=AREA_M2,
            reference_length=2.0 * RADIUS_M,
            coefficients={"cL_alpha": _lift_coefficient(cl_alpha)},
            center_of_pressure=(0.0, 0.0, 0.0),
            name="lift",
        )
        rocket.add_surfaces(surface, 0.0)
    return rocket


def _environment():
    env = Environment(latitude=LATITUDE, longitude=LONGITUDE, elevation=ELEVATION_M)
    env.set_atmospheric_model(type="standard_atmosphere")
    return env


def _fly(rocket, inclination, heading, max_time):
    return Flight(
        rocket=rocket,
        environment=_environment(),
        rail_length=0.3,
        inclination=inclination,
        heading=heading,
        max_time=max_time,
        terminate_on_apogee=False,
    )


def _series(flight, name, time):
    fn = getattr(flight, name)
    return np.array([float(fn(t)) for t in time], dtype=float)


def _attitude_to_ypr(east, north, up):
    pitch = np.arcsin(np.clip(up, -1.0, 1.0))
    yaw = np.arctan2(east, north)
    roll = np.zeros_like(pitch)
    return yaw, pitch, roll


def write_state_file(path: Path, flight, rocket, latitude=LATITUDE, longitude=LONGITUDE) -> np.ndarray:
    """Write the East-North-Up state table recover.py reads."""
    time = np.asarray(flight.time, dtype=float)
    if time.size > 160:
        index = np.unique(np.round(np.linspace(0, time.size - 1, 160)).astype(int))
        time = time[index]
    east = _series(flight, "x", time)
    north = _series(flight, "y", time)
    up = _series(flight, "z", time)
    vel_e = _series(flight, "vx", time)
    vel_n = _series(flight, "vy", time)
    vel_u = _series(flight, "vz", time)
    acc_e = _series(flight, "ax", time)
    acc_n = _series(flight, "ay", time)
    acc_u = _series(flight, "az", time)
    att_e = _series(flight, "attitude_vector_x", time)
    att_n = _series(flight, "attitude_vector_y", time)
    att_u = _series(flight, "attitude_vector_z", time)
    yaw, pitch, roll = _attitude_to_ypr(att_e, att_n, att_u)
    # The nose rebuilt from yaw and pitch must be the attitude vector the fitter uses.
    rebuilt = recover._body_axis(yaw, pitch)
    attitude = np.column_stack([att_e, att_n, att_u])
    attitude /= np.maximum(np.linalg.norm(attitude, axis=1, keepdims=True), 1e-12)
    if np.max(np.linalg.norm(rebuilt - attitude, axis=1)) > 1e-6:
        raise AssertionError("yaw/pitch did not rebuild the RocketPy nose axis")
    mass = np.array([float(rocket.total_mass(t)) for t in time], dtype=float)
    header = (
        f"# latitude_deg={latitude}\n"
        f"# longitude_deg={longitude}\n"
        "time_s,east_m,north_m,up_m,"
        "vel_east_mps,vel_north_mps,vel_up_mps,"
        "acc_east_mps2,acc_north_mps2,acc_up_mps2,"
        "yaw_rad,pitch_rad,roll_rad,mass_kg\n"
    )
    columns = np.column_stack(
        [time, east, north, up, vel_e, vel_n, vel_u, acc_e, acc_n, acc_u, yaw, pitch, roll, mass]
    )
    body = "\n".join(",".join(f"{value:.8g}" for value in row) for row in columns)
    path.write_text(header + body + "\n", encoding="utf-8")
    return time


def _thrust_curve_from(result):
    times = result["thrust"]["time_s"]
    force = result["thrust"]["thrust_n"]
    if len(times) < 2:
        raise AssertionError("recovered thrust curve is empty")
    curve = [[float(t), float(f)] for t, f in zip(times, force)]
    if curve[-1][1] > 1.0:
        curve.append([curve[-1][0] + 0.05, 0.0])
    burn = curve[-2][0] if curve[-1][1] == 0.0 else curve[-1][0]
    return curve, max(burn, 0.2)


class RecoverRoundTripTest(unittest.TestCase):
    def test_density_matches_rocketpy_standard_atmosphere(self):
        env = _environment()
        for height in (0.0, 1400.0, 3000.0, 8000.0, 15000.0):
            got = float(recover.isa_density([height])[0])
            truth = float(env.density(height))
            self.assertLess(abs(got - truth) / truth, 0.01, f"density at {height} m")

    def test_round_trip_thrust_drag_and_lift(self):
        thrust = [[0.0, 3200.0], [1.5, 3200.0], [3.0, 1800.0]]
        rocket = _rocket(CD_ON, CD_OFF, CL_ALPHA, thrust, 3.0)
        flight = _fly(rocket, INCLINATION_DEG, HEADING_DEG, 16.0)
        path = Path(__file__).resolve().parent / "_roundtrip_states.csv"
        try:
            time = write_state_file(path, flight, rocket)
            result = recover.recover_csv(path.read_text(encoding="utf-8"))
        finally:
            path.unlink(missing_ok=True)

        self.assertTrue(result["lift_observable"], result.get("lift_note"))
        cl_hat = result["cl_alpha_a_m2_per_rad"]
        cl_true = CL_ALPHA * AREA_M2
        self.assertLess(abs(cl_hat - cl_true) / cl_true, 0.05, f"Cl_alpha*A {cl_hat} vs {cl_true}")

        cd_boost = result["cd_a_m2"]["boost"]
        cd_coast = result["cd_a_m2"]["coast"]
        self.assertTrue(result["cd_a_m2"]["boost_observable"])
        self.assertTrue(result["cd_a_m2"]["coast_observable"])
        self.assertLess(abs(cd_boost - CD_ON * AREA_M2) / (CD_ON * AREA_M2), 0.08)
        self.assertLess(abs(cd_coast - CD_OFF * AREA_M2) / (CD_OFF * AREA_M2), 0.08)

        # Compare thrust on the burn, away from the exact endpoints.
        recovered_t = np.asarray(result["thrust"]["time_s"], dtype=float)
        recovered_f = np.asarray(result["thrust"]["thrust_n"], dtype=float)
        interior = (recovered_t > 0.35) & (recovered_t < 2.85)
        self.assertGreater(int(np.count_nonzero(interior)), 5)
        truth = np.array([float(flight.net_thrust(t)) for t in recovered_t[interior]])
        rms = float(np.sqrt(np.mean((recovered_f[interior] - truth) ** 2)))
        self.assertLess(rms, 80.0, f"thrust rms {rms} N")

        curve, burn = _thrust_curve_from(result)
        refly_cl = cl_hat / AREA_M2
        refly = _fly(
            _rocket(cd_boost / AREA_M2, cd_coast / AREA_M2, refly_cl, curve, burn),
            INCLINATION_DEG,
            HEADING_DEG,
            16.0,
        )
        samples = np.linspace(0.5, 15.0, 30)
        errors = []
        for sample in samples:
            dx = float(flight.x(sample) - refly.x(sample))
            dy = float(flight.y(sample) - refly.y(sample))
            dz = float(flight.z(sample) - refly.z(sample))
            errors.append(math.hypot(math.hypot(dx, dy), dz))
        peak_error = max(errors)
        # Printed for the pull-request summary.
        print(
            f"Cd*A boost {cd_boost:.6f} m² vs {CD_ON * AREA_M2:.6f} m² "
            f"({(cd_boost - CD_ON * AREA_M2) / (CD_ON * AREA_M2):+.2%}); "
            f"coast {cd_coast:.6f} m² vs {CD_OFF * AREA_M2:.6f} m² "
            f"({(cd_coast - CD_OFF * AREA_M2) / (CD_OFF * AREA_M2):+.2%})"
        )
        print(f"Cl_alpha*A {cl_hat:.6f} m²/rad vs {cl_true:.6f} ({(cl_hat - cl_true) / cl_true:+.2%})")
        print(f"thrust rms {rms:.2f} N on a {float(np.mean(truth)):.0f} N burn")
        print(f"re-fly peak position error {peak_error:.2f} m")
        self.assertLess(peak_error, 40.0, f"re-fly position error {peak_error:.1f} m")
        self.assertIn("coefficient times area", result["product_note"])
        self.assertTrue(any(phase["boosting"] and phase["atmosphere"] == "endoatmospheric" for phase in result["phases"]))
        self.assertTrue(any((not phase["boosting"]) and phase["atmosphere"] == "endoatmospheric" for phase in result["phases"]))

    def test_vertical_flight_marks_lift_not_observable(self):
        thrust = [[0.0, 4000.0], [2.5, 4000.0]]
        rocket = _rocket(0.5, 0.5, 0.0, thrust, 2.5)
        flight = _fly(rocket, 90.0, 0.0, 6.0)
        path = Path(__file__).resolve().parent / "_vertical_states.csv"
        try:
            write_state_file(path, flight, rocket)
            alpha = np.asarray(flight.angle_of_attack[:, 1], dtype=float)
            # Ignore the t=0 sample, where speed is zero and alpha is undefined.
            flying = np.asarray(flight.time, dtype=float) > 0.3
            self.assertLess(float(np.percentile(alpha[flying], 95)), 2.0)
            result = recover.recover_csv(path.read_text(encoding="utf-8"))
        finally:
            path.unlink(missing_ok=True)
        self.assertFalse(result["lift_observable"])
        self.assertIsNone(result["cl_alpha_a_m2_per_rad"])
        self.assertIn("not observable", result["lift_note"].lower())
        self.assertFalse(result["c_cross_observable"])
        self.assertIsNone(result["c_cross_m2"])
        self.assertIn("not observable", result["c_cross_note"].lower())
        self.assertIn("cylinder extrapolation", result["c_cross_note"].lower())
        self.assertIsNone(result["c_cross_assumed_m2"])
        self.assertTrue(result["cd_a_m2"]["coast_observable"])
        coast = result["cd_a_m2"]["coast"]
        self.assertLess(abs(coast - 0.5 * AREA_M2) / (0.5 * AREA_M2), 0.08)

    def test_missing_acceleration_orientation_or_mass_is_an_error(self):
        header = (
            "# latitude_deg=32.99\n"
            "time_s,east_m,north_m,up_m,vel_east_mps,vel_north_mps,vel_up_mps,"
            "acc_east_mps2,acc_north_mps2,acc_up_mps2,yaw_rad,pitch_rad,roll_rad,mass_kg\n"
        )
        row = "0,0,0,1400,0,10,40,0,0,20,0,1.2,0,20\n"
        cases = {
            "acceleration": ("acc_east_mps2", "acc_north_mps2", "acc_up_mps2"),
            "orientation": ("yaw_rad", "pitch_rad", "roll_rad"),
            "mass": ("mass_kg",),
        }
        names = [cell.strip() for cell in header.strip().splitlines()[-1].split(",")]
        values = row.strip().split("\n")[0].split(",")
        for group, drop in cases.items():
            keep = [i for i, name in enumerate(names) if name not in drop]
            text = ",".join(names[i] for i in keep) + "\n"
            text += "\n".join(",".join(values[i] for i in keep) for _ in range(5)) + "\n"
            with self.assertRaises(ValueError) as caught:
                recover.recover_csv(text)
            message = str(caught.exception)
            self.assertIn(group, message)
            self.assertIn("Nothing was guessed", message)
            self.assertNotIn("cd_a", message.lower())
        self.assertIn("acceleration", recover.missing_message(["acceleration", "mass"]))
        self.assertIn("Nothing was guessed", recover.missing_message(["orientation"]))


def _allen_perkins_table(alphas_deg, c_alpha, c_cross, cd_a, speed=220.0, mass=18.0, altitude=1400.0, latitude=LATITUDE):
    """States whose normal force is exactly C_α sinα cosα + C_cross sin²α, times q."""
    alpha = np.radians(np.asarray(alphas_deg, dtype=float))
    count = len(alpha)
    time = np.linspace(0.4, 0.4 + 0.05 * (count - 1), count)
    yaw = np.full(count, math.pi / 2.0)
    pitch = np.zeros(count)
    roll = np.zeros(count)
    body = np.column_stack([np.ones(count), np.zeros(count), np.zeros(count)])
    velocity = speed * np.column_stack([np.cos(alpha), np.zeros(count), np.sin(alpha)])
    density = float(recover.isa_density([altitude])[0])
    dynamic_pressure = 0.5 * density * speed**2
    normal = np.column_stack([np.zeros(count), np.zeros(count), np.ones(count)])
    magnitude = dynamic_pressure * (
        c_alpha * np.sin(alpha) * np.cos(alpha) + c_cross * np.sin(alpha) ** 2
    )
    force = (-dynamic_pressure * cd_a) * body - magnitude[:, None] * normal
    gravity = recover.somigliana_gravity(latitude, np.full(count, altitude))
    earth_rate = 2.0 * math.pi / 86164.1
    latitude_rad = math.radians(latitude)
    omega_north = earth_rate * math.cos(latitude_rad)
    omega_up = earth_rate * math.sin(latitude_rad)
    east, north, up = velocity[:, 0], velocity[:, 1], velocity[:, 2]
    specific = force / mass
    return {
        "latitude_deg": latitude,
        "longitude_deg": LONGITUDE,
        "time_s": time,
        "east_m": np.linspace(0.0, 800.0, count),
        "north_m": np.zeros(count),
        "up_m": np.full(count, altitude),
        "vel_east_mps": east,
        "vel_north_mps": north,
        "vel_up_mps": up,
        "acc_east_mps2": specific[:, 0] - 2.0 * (up * omega_north - north * omega_up),
        "acc_north_mps2": specific[:, 1] - 2.0 * (east * omega_up),
        "acc_up_mps2": specific[:, 2] - gravity - 2.0 * (-east * omega_north),
        "yaw_rad": yaw,
        "pitch_rad": pitch,
        "roll_rad": roll,
        "mass_kg": np.full(count, mass),
    }


class AllenPerkinsCrossTest(unittest.TestCase):
    def test_high_alpha_recovers_broadside_area(self):
        c_alpha = 0.04
        c_cross = 0.25
        table = _allen_perkins_table(np.linspace(4.0, 90.0, 40), c_alpha, c_cross, cd_a=0.01)
        result = recover.recover_states(table)
        self.assertTrue(result["c_cross_observable"], result.get("c_cross_note"))
        self.assertIn("cylinder extrapolation", result["c_cross_label"])
        self.assertIn("cylinder extrapolation", result["c_cross_note"].lower())
        fitted = result["c_cross_m2"]
        error = abs(fitted - c_cross) / c_cross
        print(f"C_cross {fitted:.6f} m² vs {c_cross:.6f} m² ({(fitted - c_cross) / c_cross:+.2%})")
        self.assertLess(error, 0.05, f"C_cross {fitted} vs {c_cross}")
        self.assertIsNone(result["c_cross_assumed_m2"])
        # The existing products are still reported. This series is a coast, so boost drag stays unset.
        self.assertTrue(result["cd_a_m2"]["coast_observable"])
        self.assertFalse(result["cd_a_m2"]["boost_observable"])

    def test_broadside_only_at_ninety_degrees(self):
        c_cross = 0.18
        table = _allen_perkins_table(np.full(12, 90.0), c_alpha=0.04, c_cross=c_cross, cd_a=0.01)
        result = recover.recover_states(table)
        self.assertTrue(result["c_cross_observable"])
        self.assertLess(abs(result["c_cross_m2"] - c_cross) / c_cross, 0.05)

    def test_near_zero_alpha_leaves_cross_unset_and_length_diameter_are_assumed(self):
        table = _allen_perkins_table(np.linspace(0.2, 6.0, 24), c_alpha=0.04, c_cross=0.25, cd_a=0.01)
        table["length_m"] = 2.0
        table["diameter_m"] = 0.07
        result = recover.recover_states(table)
        self.assertFalse(result["c_cross_observable"])
        self.assertIsNone(result["c_cross_m2"])
        self.assertIn("not observable", result["c_cross_note"].lower())
        self.assertIn("cylinder extrapolation", result["c_cross_note"].lower())
        assumed = 1.2 * 2.0 * 0.07
        self.assertAlmostEqual(result["c_cross_assumed_m2"], assumed)
        self.assertIn("assumed", result["c_cross_assumed_note"].lower())
        self.assertIn("not fit", result["c_cross_assumed_note"].lower())
        self.assertNotEqual(result["c_cross_assumed_m2"], result["c_cross_m2"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
