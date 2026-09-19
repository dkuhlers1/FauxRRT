import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "batch");
await mkdir(root, { recursive: true });

const hubs = [
  [-122.379, 37.621],
  [-73.778, 40.641],
  [-0.462, 51.47],
  [2.55, 49.01],
  [103.989, 1.359],
  [139.779, 35.552],
  [151.177, -33.946],
  [-46.473, -23.435],
];

for (let i = 0; i < 24; i += 1) {
  const [lon0, lat0] = hubs[i % hubs.length];
  const heading = (i * 37) % 360;
  const n = 80 + (i % 5) * 20;
  const lines = ["t,lat,lon,alt"];
  for (let k = 0; k < n; k += 1) {
    const u = k / (n - 1);
    const dist = u * (4 + (i % 6));
    const rad = (heading * Math.PI) / 180;
    const lat = lat0 + Math.cos(rad) * dist * 0.2;
    const lon = lon0 + Math.sin(rad) * dist * 0.25;
    const alt = 800 + Math.sin(u * Math.PI) * (4000 + i * 80);
    lines.push(`${(u * 900).toFixed(1)},${lat.toFixed(6)},${lon.toFixed(6)},${alt.toFixed(1)}`);
  }
  await writeFile(join(root, `track_${String(i + 1).padStart(2, "0")}.csv`), `${lines.join("\n")}\n`);
}

console.log(`wrote 24 tracks to ${root}`);
