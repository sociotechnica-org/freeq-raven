#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

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

console.log("claude-agent sidecar requires ANTHROPIC_API_KEY ok");
