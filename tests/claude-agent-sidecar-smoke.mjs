#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..");
const sidecar = path.join(repoRoot, "scripts", "claude-agent-sidecar.mjs");
const mockState = path.join(
  fs.mkdtempSync(path.join(os.tmpdir(), "freeq-raven-claude-agent-")),
  "mock-state.json",
);

function runSidecar(requests) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [sidecar], {
      cwd: repoRoot,
      env: {
        ...process.env,
        RAVEN_CLAUDE_AGENT_MOCK: "1",
        RAVEN_CLAUDE_AGENT_MOCK_STATE: mockState,
      },
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
  question: "Raven, remember that the launch codename is Night Library.",
  sessionContext: "alice [chat]: Raven, remember that the launch codename is Night Library.",
};

const [r1] = await runSidecar([first]);
assert.equal(r1.ok, true);
assert.equal(r1.id, "turn-1");
assert.match(r1.sessionId, /^mock-/);
assert.ok(r1.skills.includes("alexandria:ax-start"));

const second = {
  id: "turn-2",
  type: "turn",
  channel: "#alexandria",
  asker: "alice",
  source: "chat",
  question: "Raven, what did I ask you to remember?",
  sessionId: r1.sessionId,
  sessionContext: "alice [chat]: Raven, what did I ask you to remember?",
};

const [r2] = await runSidecar([second]);
assert.equal(r2.ok, true);
assert.equal(r2.sessionId, r1.sessionId);
assert.match(r2.text, /Night Library/);

console.log("claude-agent sidecar mock continuity ok");
