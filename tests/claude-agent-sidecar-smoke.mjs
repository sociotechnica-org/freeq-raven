#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ravenLatestFrameToolResult } from "../scripts/claude-agent-sidecar.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..");
const sidecar = path.join(repoRoot, "scripts", "claude-agent-sidecar.mjs");

function runSidecar(requests) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [sidecar], {
      cwd: repoRoot,
      env: Object.fromEntries(
        Object.entries(process.env).filter(([key]) => key !== "ANTHROPIC_API_KEY"),
      ),
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code !== 0) {
        reject(new Error(`sidecar exited ${code}: ${stderr}`));
        return;
      }
      resolve(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line)));
    });
    for (const request of requests) {
      child.stdin.write(`${JSON.stringify(request)}\n`);
    }
    child.stdin.end();
  });
}

const first = {
  id: "turn-1",
  type: "turn",
  channel: "#alexandria",
  asker: "alice",
  source: "chat",
  question: "Raven, are you connected to Claude?",
  sessionContext: "alice [chat]: Raven, are you connected to Claude?",
};

const [r1] = await runSidecar([first]);
assert.equal(r1.id, "turn-1");
assert.equal(r1.ok, false);
assert.equal(r1.error, "ANTHROPIC_API_KEY is required for Claude Agent SDK sidecar");
assert.equal(r1.text, "");

const bridgeRequests = [];
const bridge = createServer((req, res) => {
  let body = "";
  req.on("data", (chunk) => {
    body += chunk;
  });
  req.on("end", () => {
    bridgeRequests.push({
      method: req.method,
      url: req.url,
      authorization: req.headers.authorization,
      body: JSON.parse(body),
    });
    res.setHeader("content-type", "application/json");
    res.end(
      JSON.stringify({
        ok: false,
        reason: "no_frame",
        channel: "#alexandria",
        participant: "alice",
      }),
    );
  });
});
await new Promise((resolve) => bridge.listen(0, "127.0.0.1", resolve));
try {
  const { port } = bridge.address();
  const toolResult = await ravenLatestFrameToolResult(
    {
      endpoint: `http://127.0.0.1:${port}`,
      bearerToken: "test-token",
      channel: "#alexandria",
      asker: "alice",
    },
    {},
  );
  assert.equal(bridgeRequests.length, 1);
  assert.equal(bridgeRequests[0].method, "POST");
  assert.equal(bridgeRequests[0].url, "/latest-frame");
  assert.equal(bridgeRequests[0].authorization, "Bearer test-token");
  assert.deepEqual(bridgeRequests[0].body, {
    channel: "#alexandria",
    asker: "alice",
  });
  assert.deepEqual(toolResult.structuredContent, {
    ok: false,
    reason: "no_frame",
    channel: "#alexandria",
    participant: "alice",
  });
  assert.equal(toolResult.content[0].type, "text");
  assert.match(toolResult.content[0].text, /no_frame/);
} finally {
  await new Promise((resolve) => bridge.close(resolve));
}

console.log("claude-agent sidecar smoke ok");
