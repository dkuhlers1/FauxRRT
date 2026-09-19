# FauxRRT

Desktop trajectory viewer: a Cesium globe in a side panel, plus fast loading of many columnar text files.

The UI is a Tauri 2 app. Rust parses files in parallel, auto-detects the schema, and sends a downsampled lon/lat/alt polyline to Cesium.

## Run

Needs Node.js, Rust, and WebView2 (already on Windows 10/11).

```bash
npm install
npm run tauri dev
```

Release build:

```bash
npm run tauri build
```

## Load data

- **Load files** — pick one or more `.csv`, `.txt`, `.tsv`, `.dat`, `.traj`, `.eph`, `.asc`
- **Load folder** — walk a directory (up to 4000 files) and parse matches in parallel
- **Demo tracks** — synthetic air, LEO, and ground paths if you just want to see the globe

Sample files are in `samples/`. Use **Load folder** on that directory to try mixed schemas at once.

## Schema detection

FauxRRT inspects a sample of each file and guesses:

- delimiter: comma, tab, semicolon, pipe, or whitespace
- header vs raw numeric rows
- comment lines (`#`, `%`, `!`, `//`, STK-style banners)
- frame: geodetic lat/lon/alt or ECEF X/Y/Z
- time column: names, ISO-8601, Unix seconds/millis, or a monotonic first column

Name aliases include `lat`/`latitude`, `lon`/`long`/`lng`, `alt`/`height`/`hae`, `x_ecef`/`ecef_x`, `epoch`/`utc`/`timestamp`.

If a guess is wrong, select the track and change the column mapping, then **Reparse with mapping**.

## Performance

- Files are memory-read and parsed on a Rayon thread pool
- Only the mapped columns are extracted
- Display uses Largest-Triangle-Three-Buckets downsampling against a global vertex budget (~360k points), so hundreds of tracks stay interactive
- Cesium draws `PolylineCollection` primitives, not per-point entities

## Layout

Left panel: file actions, track list, detected schema.  
Right panel: Cesium globe. Drag the splitter to resize.
