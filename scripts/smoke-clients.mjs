import { TinyLogClient } from "../server/web/tinylog.mjs";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const TinyLogBrowser = require("../server/web/tinylog.js");

const baseUrl = process.env.TINY_LOG_URL || "http://127.0.0.1:8080";
const apiKey = process.env.TINY_LOG_API_KEY || "dev-api-key";
const clientToken = process.env.TINY_LOG_CLIENT_TOKEN || "dev-client-token";

let failed = false;
function assert(cond, msg) {
  if (!cond) {
    console.error("FAIL", msg);
    failed = true;
  }
}

const node = new TinyLogClient({
  baseUrl,
  apiKey,
  app: "node-smoke",
  source: "node-smoke",
});

await node.log("info", "node helper smoke", { ok: true });
assert(node.status === "ok", `node status=${node.status}`);

await node.sendBatch([
  { level: "info", message: "batch-1" },
  { level: "warn", message: "batch-2" },
]);
assert(node.status === "ok", `node batch status=${node.status}`);

const bad = new TinyLogClient({
  baseUrl,
  apiKey: "wrong-key",
  app: "node-smoke",
  maxRetries: 1,
});
let threw = false;
try {
  await bad.log("info", "should fail");
} catch {
  threw = true;
}
assert(threw && bad.status === "unable", `bad key status=${bad.status}`);

const browser = new TinyLogBrowser({
  baseUrl,
  clientToken,
  app: "browser-smoke",
  source: "browser-smoke",
});
await browser.log("info", "browser helper smoke", { ok: true });
assert(browser.status === "ok", `browser status=${browser.status}`);

const badBrowser = new TinyLogBrowser({
  baseUrl,
  clientToken: "wrong-token",
  app: "browser-smoke",
  maxRetries: 1,
});
threw = false;
try {
  await badBrowser.log("info", "should fail");
} catch {
  threw = true;
}
assert(threw && badBrowser.status === "unable", `bad browser status=${badBrowser.status}`);

if (failed) process.exit(1);
console.log("OK node+browser helpers");
