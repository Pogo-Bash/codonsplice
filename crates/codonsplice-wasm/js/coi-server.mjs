// Minimal static file server that sends the COOP/COEP headers required for
// cross-origin isolation (so SharedArrayBuffer — hence the WASM worker pool — is
// available). Use it to run verify-coi.html locally:
//
//   node crates/codonsplice-wasm/js/coi-server.mjs
//   -> http://localhost:8080/crates/codonsplice-wasm/js/verify-coi.html
//
// The two headers below are the EXACT contract a production host (CDN, reverse
// proxy, static server) must replicate on the app's routes. Without them,
// self.crossOriginIsolated === false and the pool falls back to serial.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

const ROOT = process.cwd();
const PORT = process.env.PORT || 8080;

const MIME = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".wasm": "application/wasm",
  ".json": "application/json",
  ".bam": "application/octet-stream",
  ".bai": "application/octet-stream",
};

createServer(async (req, res) => {
  // The cross-origin isolation contract (PARALLELISM_WASM.md §3):
  res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
  res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  res.setHeader("Cross-Origin-Resource-Policy", "same-origin");

  try {
    const path = normalize(join(ROOT, decodeURIComponent(new URL(req.url, "http://x").pathname)));
    if (!path.startsWith(ROOT)) {
      res.writeHead(403).end("forbidden");
      return;
    }
    const body = await readFile(path);
    res.writeHead(200, { "Content-Type": MIME[extname(path)] || "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
}).listen(PORT, () => {
  console.log(`COI server (COOP same-origin + COEP require-corp) on http://localhost:${PORT}`);
});
