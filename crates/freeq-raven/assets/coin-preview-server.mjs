#!/usr/bin/env node
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(ROOT, "../../..");
const PORT = Number(process.env.PORT || 8765);
const HOST = process.env.HOST || "127.0.0.1";
const DEFAULT_VOICE = "aj0fZfXTBc7E3By4X8L2";
const MODEL = process.env.ELEVENLABS_MODEL || "eleven_turbo_v2_5";

await loadEnvFiles([
  process.env.RAVEN_ENV_FILE,
  join(REPO_ROOT, ".env"),
]);

const types = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".mp4", "video/mp4"],
  [".png", "image/png"],
  [".ico", "image/x-icon"],
]);

function send(res, status, body, headers = {}) {
  res.writeHead(status, headers);
  res.end(body);
}

async function loadEnvFiles(paths) {
  for (const path of paths.filter(Boolean)) {
    let text;
    try {
      text = await readFile(path, "utf8");
    } catch {
      continue;
    }
    for (const line of text.split(/\r?\n/)) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith("#")) continue;
      const match = trimmed.match(/^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/);
      if (!match || process.env[match[1]]) continue;
      let value = match[2].trim();
      if (
        (value.startsWith('"') && value.endsWith('"')) ||
        (value.startsWith("'") && value.endsWith("'"))
      ) {
        value = value.slice(1, -1);
      }
      process.env[match[1]] = value;
    }
  }
}

async function readBody(req) {
  let body = "";
  for await (const chunk of req) {
    body += chunk;
    if (body.length > 10_000) {
      throw new Error("request too large");
    }
  }
  return body;
}

async function handleVoice(req, res) {
  let data;
  try {
    data = JSON.parse(await readBody(req));
  } catch {
    send(res, 400, "Invalid JSON request.");
    return;
  }

  const apiKey = String(req.headers["x-elevenlabs-api-key"] || process.env.ELEVENLABS_API_KEY || "")
    .trim();
  const text = String(data.text || "").trim().slice(0, 500) || "Raven online.";

  if (!apiKey) {
    send(res, 400, "Set ELEVENLABS_API_KEY in .env before starting the preview server.");
    return;
  }

  const voiceId = String(data.voiceId || DEFAULT_VOICE).trim() || DEFAULT_VOICE;
  const url = `https://api.elevenlabs.io/v1/text-to-speech/${encodeURIComponent(voiceId)}?output_format=mp3_44100_128`;
  const upstream = await fetch(url, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "xi-api-key": apiKey,
    },
    body: JSON.stringify({
      text,
      model_id: MODEL,
      voice_settings: {
        stability: 0.7,
        similarity_boost: 0.75,
        speed: 1.02,
      },
    }),
  });

  if (!upstream.ok) {
    const detail = await upstream.text();
    send(res, upstream.status, detail || `ElevenLabs returned ${upstream.status}.`);
    return;
  }

  const bytes = Buffer.from(await upstream.arrayBuffer());
  send(res, 200, bytes, {
    "content-type": "audio/mpeg",
    "cache-control": "no-store",
    "x-voice-source": "elevenlabs",
  });
}

async function handleStatic(req, res) {
  const requestUrl = new URL(req.url || "/", `http://${HOST}:${PORT}`);
  const pathname = requestUrl.pathname === "/" ? "/coin-preview.html" : requestUrl.pathname;
  const filepath = resolve(ROOT, `.${pathname}`);
  if (!filepath.startsWith(`${ROOT}/`) && filepath !== ROOT) {
    send(res, 403, "Forbidden.");
    return;
  }

  try {
    const bytes = await readFile(filepath);
    send(res, 200, bytes, {
      "content-type": types.get(extname(filepath)) || "application/octet-stream",
      "cache-control": "no-cache",
    });
  } catch {
    send(res, 404, "Not found.");
  }
}

const server = createServer(async (req, res) => {
  try {
    if (req.method === "POST" && req.url?.startsWith("/voice-test")) {
      await handleVoice(req, res);
      return;
    }
    if (req.method === "GET" || req.method === "HEAD") {
      await handleStatic(req, res);
      return;
    }
    send(res, 405, "Method not allowed.");
  } catch (error) {
    send(res, 500, error instanceof Error ? error.message : "Preview server failed.");
  }
});

server.listen(PORT, HOST, () => {
  console.log(`Raven coin preview: http://${HOST}:${PORT}/coin-preview.html`);
  console.log("Raven voice uses ELEVENLABS_API_KEY from .env or RAVEN_ENV_FILE.");
});
