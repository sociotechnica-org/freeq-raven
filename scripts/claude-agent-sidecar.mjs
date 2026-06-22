#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const DEFAULT_ALLOWED_TOOLS = [
  "Read",
  "Glob",
  "Grep",
  "WebSearch",
  "WebFetch",
  "Bash(ax *)",
  "Bash(./.alexandria-next/bin/ax *)",
  "Bash(.alexandria-next/bin/ax *)",
  "Bash(.alexandria/bin/ax *)",
];

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");
const VISION_MCP_SERVER_NAME = "raven_vision";
const VISION_TOOL_NAME = "raven_latest_frame";

function readStdin() {
  return fs.readFileSync(0, "utf8");
}

function parseRequests(input) {
  const trimmed = input.trim();
  if (!trimmed) return [];
  if (trimmed.startsWith("{")) return [JSON.parse(trimmed)];
  return trimmed
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

function jsonLine(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function trace(event, fields = {}) {
  if (process.env.RAVEN_AGENT_TRACE === "0") return;
  process.stderr.write(
    `${JSON.stringify({
      event: `claude_agent.${event}`,
      ts: new Date().toISOString(),
      ...fields,
    })}\n`,
  );
}

function textBlocks(message) {
  const content = message?.message?.content ?? [];
  return content
    .filter((block) => block?.type === "text" && typeof block.text === "string")
    .map((block) => block.text)
    .join("");
}

function buildPrompt(req) {
  const source = req.source || "room";
  const asker = req.asker || "participant";
  const context = (req.sessionContext || "").trim() || "(no room context yet)";
  const lines = [
    `Freeq channel: ${req.channel || "(unknown)"}`,
    `Latest addressed ${source} turn from ${asker}:`,
    req.question || "",
    "",
    "Recent normalized room context:",
    context,
  ];
  if (req.visionBridge) {
    const participants = Array.isArray(req.visionBridge.participants)
      ? req.visionBridge.participants
      : [];
    const summary = participants.length
      ? participants
          .map((participant) => {
            const name = participant?.name || "unknown";
            if (participant?.frameAvailable) return `${name} (fresh frame available)`;
            if (participant?.frameStale) return `${name} (stale frame, do not assume visible)`;
            return `${name} (no current frame yet)`;
          })
          .join(", ")
      : "(no visible participants registered yet)";
    lines.push("", "Vision bridge participants:", summary);
  }
  if (req.silentAllowed) {
    lines.push(
      "",
      "Response contract for this candidate wake follow-up:",
      'Return exactly one JSON object and no Markdown: {"action":"ignore","text":""} when the chat is unrelated, stale, already handled, or needs no public room reply.',
      'Return exactly one JSON object and no Markdown: {"action":"reply","text":"..."} only when Raven should say the text publicly in the room.',
      'If you take an internal action but the room does not need an update, use {"action":"ignore","text":""}.',
    );
  }
  return lines.join("\n");
}

function buildSystemPromptAppend(req) {
  const parts = [];
  if (req.systemPrompt) parts.push(req.systemPrompt);
  if (req.visionBridge) {
    parts.push(
      [
        "Raven vision tool: you have an MCP tool named `raven_latest_frame` for inspecting the latest visible screen/camera frame in this Freeq channel.",
        "Use it only when the user is actually asking about visual content, a screen, a camera, an image, or something currently visible.",
        'Do not call it merely because the user uses phrases like "looking at" in a non-visual sentence.',
        "If the tool reports `no_active_call`, `no_frame`, `unknown_participant`, or `stale_frame`, answer naturally from that fact instead of inventing visual details.",
        "For ordinary chat, code, planning, or room-context questions, continue without using the vision tool.",
      ].join("\n"),
    );
  }
  return parts.join("\n\n");
}

function defaultAllowedTools(req) {
  const tools = [...DEFAULT_ALLOWED_TOOLS];
  if (req.visionBridge) {
    tools.push(`mcp__${VISION_MCP_SERVER_NAME}__${VISION_TOOL_NAME}`);
  }
  return tools;
}

function discoverAlexandriaPlugin(cwd, explicitPath) {
  const candidates = [
    explicitPath,
    process.env.RAVEN_ALEXANDRIA_PLUGIN_PATH,
    path.join(cwd, ".claude", "plugins", "alexandria"),
    path.join(REPO_ROOT, ".claude", "plugins", "alexandria"),
    path.join(process.env.HOME || "", ".claude", "plugins", "alexandria"),
  ].filter(Boolean);

  for (const candidate of candidates) {
    const full = path.resolve(cwd, candidate);
    if (
      fs.existsSync(path.join(full, ".claude-plugin")) ||
      fs.existsSync(path.join(full, "skills")) ||
      fs.existsSync(path.join(full, "SKILL.md"))
    ) {
      return full;
    }
  }
  return null;
}

function baseResponse(req, overrides) {
  return {
    id: req.id ?? null,
    type: "response",
    ok: true,
    action: "reply",
    text: "",
    sessionId: null,
    plugins: [],
    skills: [],
    slashCommands: [],
    ...overrides,
  };
}

function parseCandidateDecision(text) {
  const trimmed = (text || "").trim();
  if (!trimmed) return { action: "ignore", text: "" };

  const fenced = trimmed.match(/^```(?:json)?\s*([\s\S]*?)\s*```$/i);
  const payload = fenced ? fenced[1].trim() : trimmed;
  try {
    const parsed = JSON.parse(payload);
    const action = parsed?.action === "ignore" ? "ignore" : "reply";
    const replyText =
      typeof parsed?.text === "string"
        ? parsed.text.trim()
        : typeof parsed?.reply === "string"
          ? parsed.reply.trim()
          : "";
    return {
      action: action === "ignore" || !replyText ? "ignore" : "reply",
      text: action === "ignore" ? "" : replyText,
    };
  } catch {
    return { action: "reply", text: trimmed };
  }
}

function normalizePlugins(plugins) {
  if (!Array.isArray(plugins)) return [];
  return plugins.map((plugin) => {
    if (typeof plugin === "string") return { name: plugin, path: "" };
    return {
      name: plugin?.name || plugin?.id || plugin?.path || "unknown",
      path: plugin?.path || "",
    };
  });
}

function normalizeNames(values) {
  if (!Array.isArray(values)) return [];
  return values
    .map((value) => {
      if (typeof value === "string") return value;
      return value?.name || value?.id || value?.command || null;
    })
    .filter((value) => typeof value === "string" && value.length > 0);
}

function summarizeContent(content) {
  if (!Array.isArray(content)) {
    return { blocks: 0, blockTypes: [], textChars: 0, toolNames: [] };
  }
  const blockTypes = [];
  const toolNames = [];
  let textChars = 0;
  for (const block of content) {
    if (!block || typeof block !== "object") continue;
    if (typeof block.type === "string") blockTypes.push(block.type);
    if (block.type === "text" && typeof block.text === "string") {
      textChars += block.text.length;
    }
    if (typeof block.name === "string") toolNames.push(block.name);
  }
  return { blocks: content.length, blockTypes, textChars, toolNames };
}

function requireAnthropicApiKey() {
  if (!process.env.ANTHROPIC_API_KEY || !process.env.ANTHROPIC_API_KEY.trim()) {
    throw new Error("ANTHROPIC_API_KEY is required for Claude Agent SDK sidecar");
  }
}

function dataUriToImageContent(dataUri, fallbackMime) {
  if (typeof dataUri !== "string") return null;
  const match = dataUri.match(/^data:([^;,]+);base64,(.*)$/s);
  if (!match) return null;
  return {
    type: "image",
    data: match[2],
    mimeType: match[1] || fallbackMime || "image/jpeg",
  };
}

function compactVisionResult(result) {
  if (!result || typeof result !== "object") {
    return { ok: false, reason: "invalid_bridge_response" };
  }
  const { dataUri: _dataUri, ...rest } = result;
  return rest;
}

function visionResultText(result) {
  const compact = compactVisionResult(result);
  if (result?.ok) {
    const dims = Array.isArray(result.dimensions)
      ? `${result.dimensions[0]}x${result.dimensions[1]}`
      : "unknown dimensions";
    return [
      `Latest visible frame for ${result.participant || "the participant"} is available.`,
      `Metadata: ${JSON.stringify({ ...compact, dimensionsText: dims })}`,
    ].join("\n");
  }
  return `No visible frame is available. Bridge result: ${JSON.stringify(compact)}`;
}

export function visionBridgeResultToToolResult(result) {
  const content = [{ type: "text", text: visionResultText(result) }];
  if (result?.ok) {
    const image = dataUriToImageContent(result.dataUri, result.mime);
    if (image) content.push(image);
  }
  return {
    content,
    structuredContent: compactVisionResult(result),
  };
}

export async function fetchVisionBridgeFrame(visionBridge, args = {}) {
  if (!visionBridge?.endpoint || !visionBridge?.bearerToken) {
    return { ok: false, reason: "vision_bridge_unavailable" };
  }
  const endpoint = String(visionBridge.endpoint).replace(/\/+$/, "");
  const body = {
    channel: visionBridge.channel,
    asker: visionBridge.asker,
  };
  if (typeof args.participant === "string" && args.participant.trim()) {
    body.participant = args.participant.trim();
  }
  if (typeof args.question === "string" && args.question.trim()) {
    body.question = args.question.trim();
  }
  const response = await fetch(`${endpoint}/latest-frame`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${visionBridge.bearerToken}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  });
  const responseText = await response.text();
  const parsed = (() => {
    try {
      return JSON.parse(responseText);
    } catch {
      return {
        ok: false,
        reason: "invalid_bridge_response",
        status: response.status,
        text: responseText,
      };
    }
  })();
  if (parsed && typeof parsed.ok === "boolean") return parsed;
  return {
    ok: false,
    reason: "invalid_bridge_response",
    status: response.status,
    response: parsed,
  };
}

export async function ravenLatestFrameToolResult(visionBridge, args = {}) {
  const result = await fetchVisionBridgeFrame(visionBridge, args);
  return visionBridgeResultToToolResult(result);
}

function createVisionMcpServer(req, sdk) {
  if (!req.visionBridge) return null;
  const { createSdkMcpServer, tool, z } = sdk;
  if (!createSdkMcpServer || !tool || !z) return null;
  return createSdkMcpServer({
    name: VISION_MCP_SERVER_NAME,
    version: "0.1.0",
    alwaysLoad: true,
    instructions:
      "Provides Raven with the current Freeq screen/camera frame on demand. Use only for genuinely visual user requests.",
    tools: [
      tool(
        VISION_TOOL_NAME,
        "Inspect the latest visible screen/camera frame for the current Freeq channel. Defaults to the current asker.",
        {
          participant: z
            .string()
            .optional()
            .describe("Optional participant name. Defaults to the current asker."),
          question: z
            .string()
            .optional()
            .describe("Optional visual question or focus for the inspection."),
        },
        async (args) => ravenLatestFrameToolResult(req.visionBridge, args),
        { alwaysLoad: true },
      ),
    ],
  });
}

async function handleReal(req) {
  requireAnthropicApiKey();
  const sdk = await import("@anthropic-ai/claude-agent-sdk");
  const { query } = sdk;
  const cwd = path.resolve(req.cwd || process.cwd());
  const pluginPath = discoverAlexandriaPlugin(cwd, req.alexandriaPluginPath);
  const plugins = [];
  if (pluginPath) {
    plugins.push({ type: "local", path: pluginPath });
  }
  const visionMcpServer = createVisionMcpServer(req, sdk);
  const mcpServers = visionMcpServer
    ? { [VISION_MCP_SERVER_NAME]: visionMcpServer }
    : undefined;

  const options = {
    cwd,
    maxTurns: req.maxTurns ?? 24,
    model: req.model || process.env.RAVEN_AGENT_MODEL || undefined,
    allowedTools: req.allowedTools || defaultAllowedTools(req),
    permissionMode: req.permissionMode || process.env.RAVEN_AGENT_PERMISSION_MODE || "dontAsk",
    mcpServers,
    plugins,
    skills: "all",
    systemPrompt: {
      type: "preset",
      preset: "claude_code",
      append: buildSystemPromptAppend(req),
    },
    title: req.title || `Raven ${req.channel || "room"}`,
    env: {
      ...process.env,
      CLAUDE_AGENT_SDK_CLIENT_APP: "freeq-raven/0.1.0",
      ALEXANDRIA_CLAUDE_CONNECTION_ID:
        process.env.ALEXANDRIA_CLAUDE_CONNECTION_ID ||
        `host:claude-code:freeq-raven:${(req.channel || "default").replace(/[^a-zA-Z0-9_-]/g, "_")}`,
    },
  };
  if (req.sessionId) options.resume = req.sessionId;
  if (options.permissionMode === "bypassPermissions") {
    options.allowDangerouslySkipPermissions = true;
  }

  trace("turn_start", {
    channel: req.channel || null,
    source: req.source || null,
    asker: req.asker || null,
    cwd,
    maxTurns: options.maxTurns,
    model: options.model || null,
    resume: Boolean(req.sessionId),
    pluginPath: pluginPath || null,
    visionBridge: Boolean(req.visionBridge),
  });

  let resultMessage = null;
  let initMessage = null;
  let assistantText = "";
  for await (const message of query({ prompt: buildPrompt(req), options })) {
    if (message.type === "system" && message.subtype === "init") {
      initMessage = message;
      trace("init", {
        sessionId: message.session_id || null,
        model: message.model || null,
        plugins: normalizePlugins(message.plugins).map((plugin) => plugin.name),
        skills: normalizeNames(message.skills),
        slashCommands: normalizeNames(message.slash_commands),
      });
    } else if (message.type === "assistant") {
      trace("assistant", summarizeContent(message?.message?.content));
      assistantText += textBlocks(message);
    } else if (message.type === "user") {
      trace("user", summarizeContent(message?.message?.content));
    } else if (message.type === "result") {
      resultMessage = message;
      trace("result", {
        subtype: message.subtype || null,
        sessionId: message.session_id || null,
        resultChars:
          typeof message.result === "string" ? message.result.length : 0,
        totalCostUsd: message.total_cost_usd ?? null,
        durationMs: message.duration_ms ?? null,
      });
    } else {
      trace("event", {
        type: message?.type || null,
        subtype: message?.subtype || null,
      });
    }
  }

  if (!resultMessage) {
    throw new Error("Claude Agent SDK returned no result message");
  }
  if (resultMessage.subtype !== "success") {
    trace("turn_finish", {
      ok: false,
      error: resultMessage.subtype || "claude_agent_error",
      textChars: (resultMessage.result || assistantText || "").length,
      sessionId: resultMessage.session_id || initMessage?.session_id || req.sessionId || null,
    });
    return baseResponse(req, {
      ok: false,
      error: resultMessage.subtype || "claude_agent_error",
      text: resultMessage.result || assistantText || "",
      sessionId: resultMessage.session_id || initMessage?.session_id || req.sessionId || null,
      plugins: normalizePlugins(initMessage?.plugins),
      skills: normalizeNames(initMessage?.skills),
      slashCommands: normalizeNames(initMessage?.slash_commands),
    });
  }

  trace("turn_finish", {
    ok: true,
    textChars: (resultMessage.result || assistantText || "").length,
    sessionId: resultMessage.session_id || initMessage?.session_id || req.sessionId || null,
  });

  const rawText = resultMessage.result || assistantText;
  const decision = req.silentAllowed
    ? parseCandidateDecision(rawText)
    : { action: "reply", text: rawText };

  return baseResponse(req, {
    action: decision.action,
    text: decision.text,
    sessionId: resultMessage.session_id || initMessage?.session_id || req.sessionId || null,
    plugins: normalizePlugins(initMessage?.plugins),
    skills: normalizeNames(initMessage?.skills),
    slashCommands: normalizeNames(initMessage?.slash_commands),
    model: initMessage?.model,
  });
}

async function handle(req) {
  if (!req || req.type !== "turn") {
    return baseResponse(req || {}, {
      ok: false,
      error: "unsupported_request",
      text: "",
    });
  }
  return handleReal(req);
}

async function main() {
  const requests = parseRequests(readStdin());
  for (const req of requests) {
    try {
      jsonLine(await handle(req));
    } catch (error) {
      jsonLine(
        baseResponse(req, {
          ok: false,
          error: error?.message || String(error),
          stack: process.env.RAVEN_AGENT_DEBUG === "1" ? error?.stack : undefined,
        }),
      );
    }
  }
}

function isMainModule() {
  return process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
}

if (isMainModule()) {
  main().catch((error) => {
    console.error(error?.stack || error);
    process.exit(1);
  });
}
