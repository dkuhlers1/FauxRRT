# FauxRRT

Desktop RCC-321 risk tool: load or generate trajectories, assign them to objects and failure modes, and view impacts, KDE, and debris on a Cesium globe.

This is a **Windows** Tauri 2 app (Rust backend + web frontend). The globe loads Cesium from the internet, so stay online the first time you run it.

## Run from source

### Prerequisites

Install these before cloning:

1. **[Git](https://git-scm.com/download/win)**
2. **[Node.js 20 LTS](https://nodejs.org/)** (or newer)
3. **[Rust](https://rustup.rs/)** (stable). This repo includes `rust-toolchain.toml`, so `rustup` will pick the right toolchain.
4. **[Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)** with the **Desktop development with C++** workload. Tauri and the Rust compiler need the MSVC linker on Windows.
5. **WebView2** — already on Windows 10/11.

Optional, only for **6DOF** (RocketPy) trajectories:

- **[Python 3.10+](https://www.python.org/downloads/)** with **Add python.exe to PATH** checked. The first 6DOF run creates `rocketpy_backend/.venv` and installs `rocketpy`.

See also the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).

### Clone and start

```powershell
git clone https://github.com/dkuhlers1/FauxRRT.git
cd FauxRRT
npm install
npm run tauri dev
```

The first `tauri dev` compiles Rust and can take several minutes. After that, the FauxRRT window should open. Leave the terminal running while you use the app.

If `npm` or `cargo` is not found, close the terminal and open a new one so PATH updates from the installers take effect.

### Release build

```powershell
npm run tauri build
```

The installer is written under `src-tauri/target/release/bundle/` (or your `CARGO_TARGET_DIR` if you set one).

## Load data

- **Load files** — `.csv`, `.txt`, `.tsv`, `.dat`, `.traj`, `.eph`, `.asc`
- **Load folder** — walks a directory for matching files
- Sample tracks are in `samples/`. Load that folder to try mixed schemas.
- Missions save as `.fauxrrt` files.

## 6DOF (optional)

The in-app 6DOF builder uses Python + RocketPy. With Python on PATH, FauxRRT bootstraps a venv on first use.

To prepare it yourself:

```powershell
python -m venv rocketpy_backend\.venv
rocketpy_backend\.venv\Scripts\python -m pip install -r rocketpy_backend\requirements.txt
```

To force a specific interpreter, set `FAUXRRT_PYTHON` to the `python.exe` path and restart the app.

## OneDrive / synced folders

If this repo lives in OneDrive, Cargo can fail because `target/` is synced or marked read-only. Point the build directory at a local folder before `npm run tauri dev`:

```powershell
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\fauxrrt-cargo-target"
npm run tauri dev
```

## Schema detection

FauxRRT samples each file and guesses delimiter, header vs numeric rows, comment lines, geodetic vs ECEF, and a time column. If a guess is wrong, select the track, change the column mapping, then **Reparse with mapping**.
