import { readFile, writeFile } from "node:fs/promises";

const entry = new URL("../swift-host/Assets/index.html", import.meta.url);
const html = await readFile(entry, "utf8");
const scripts = [...html.matchAll(/<script type="module" crossorigin src="(\/assets\/[a-zA-Z0-9_.-]+\.js)"><\/script>/g)];
if (scripts.length !== 1) throw new Error("The Swift manager requires one local, self-contained script.");
const [tag, src] = scripts[0];
await writeFile(entry, html.replace(tag, '<script defer src="' + src + '"></script>').replace(/ crossorigin(?=[ >])/g, ""));
