const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const COURSEWARE_PREVIEW_BASE_URL =
  "https://bloom-inner.seewo.com/ai-workspace/seewo-claw/courseware-preview";
const DRAFT_STORAGE_KEY = "seewo-claw-draft-result";
const RESULT_STORAGE_KEY = "seewo-claw-result";

// ---------------------------------------------------------------------------
// Simple utilities
// ---------------------------------------------------------------------------

function parsePositiveInt(value, fallback) {
  const parsed = Number.parseInt(String(value || ""), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function parseBooleanFlag(value) {
  const normalized = String(value || "")
    .trim()
    .toLowerCase();
  return normalized === "1" || normalized === "true";
}

function logStep(message) {
  process.stderr.write(`[seewo-claw] ${message}\n`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function ensureDir(dirPath) {
  fs.mkdirSync(dirPath, { recursive: true });
}

// ---------------------------------------------------------------------------
// Playwright CLI resolution (Windows .cmd shim handling)
// ---------------------------------------------------------------------------

function resolvePlaywrightCli(cliPath) {
  if (process.platform !== "win32") {
    return { command: cliPath, prefixArgs: [] };
  }
  let resolvedCmd;
  try {
    const { execSync } = require("child_process");
    resolvedCmd = execSync(`where ${cliPath}`, { encoding: "utf8" })
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l.endsWith(".cmd"));
  } catch (_) {
    return { command: cliPath, prefixArgs: [] };
  }
  if (!resolvedCmd) {
    return { command: cliPath, prefixArgs: [] };
  }
  const cmdContent = fs.readFileSync(resolvedCmd, "utf8");
  const match = cmdContent.match(/"%_prog%"\s+"([^"]+)"/);
  if (match) {
    const jsPath = match[1].replace(/%dp0%/g, path.dirname(resolvedCmd) + "\\");
    if (fs.existsSync(jsPath)) {
      return { command: process.execPath, prefixArgs: [jsPath] };
    }
  }
  return { command: cliPath, prefixArgs: [] };
}

// ---------------------------------------------------------------------------
// Runtime dependency check
// ---------------------------------------------------------------------------

function assertRuntimeDependencies(playwrightCli) {
  const nodeMajor = Number.parseInt(process.versions.node.split(".")[0], 10);
  if (!Number.isFinite(nodeMajor) || nodeMajor < 18) {
    throw new Error(
      `当前 Node.js 版本过低: ${process.version}。请升级到 Node.js 18 或更高版本。`,
    );
  }

  const versionCheck = spawnSync(
    playwrightCli.command,
    [...playwrightCli.prefixArgs, "--version"],
    { encoding: "utf8", env: process.env },
  );

  if (versionCheck.error) {
    if (versionCheck.error.code === "ENOENT") {
      throw new Error(
        `未找到 ${playwrightCli.command}。请先安装 playwright-cli，或通过 SEEWO_CLAW_PLAYWRIGHT_CLI 指向可执行文件路径。`,
      );
    }
    throw new Error(
      `检查 ${playwrightCli.command} 可用性时失败: ${versionCheck.error.message || String(versionCheck.error)}`,
    );
  }

  if (versionCheck.status !== 0) {
    const details = [versionCheck.stderr, versionCheck.stdout]
      .filter(Boolean)
      .join("\n")
      .trim();
    throw new Error(
      `执行 ${playwrightCli.command} --version 失败。` +
        (details ? ` 输出信息: ${details}` : ""),
    );
  }
}

// ---------------------------------------------------------------------------
// CLI execution
// ---------------------------------------------------------------------------

async function runCli(playwrightCli, args, timeoutMs) {
  return new Promise((resolve, reject) => {
    const child = spawn(
      playwrightCli.command,
      [...playwrightCli.prefixArgs, ...args],
      { stdio: ["ignore", "pipe", "pipe"], env: process.env },
    );

    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => {
      child.kill("SIGTERM");
      reject(
        new Error(`命令执行超时: ${playwrightCli.command} ${args.join(" ")}`),
      );
    }, timeoutMs);

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });

    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });

    child.on("error", (error) => {
      clearTimeout(timer);
      if (error && error.code === "ENOENT") {
        reject(
          new Error(
            `未找到 ${playwrightCli.command}。请先安装 playwright-cli，或通过 SEEWO_CLAW_PLAYWRIGHT_CLI 指向可执行文件路径。`,
          ),
        );
        return;
      }
      reject(error);
    });

    child.on("close", (code) => {
      clearTimeout(timer);
      if (code === 0) {
        resolve(stdout);
        return;
      }
      const details = [stderr.trim(), stdout.trim()].filter(Boolean).join("\n");
      reject(new Error(details || `命令退出码 ${code}`));
    });
  });
}

// ---------------------------------------------------------------------------
// Session management
// ---------------------------------------------------------------------------

async function closeSessionQuietly(
  playwrightCli,
  sessionName,
  timeoutMs,
  debugMode,
) {
  if (debugMode) {
    return;
  }
  try {
    await runCli(playwrightCli, ["-s=" + sessionName, "close"], timeoutMs);
  } catch (_) {
    // 关闭失败不影响主流程
  }
}

// ---------------------------------------------------------------------------
// Result polling
// ---------------------------------------------------------------------------

function buildPollResultRunCode() {
  return `
    async (page) => {
      const draftStorageKey = ${JSON.stringify(DRAFT_STORAGE_KEY)};
      const resultStorageKey = ${JSON.stringify(RESULT_STORAGE_KEY)};
      const payload = await page.evaluate(() => ({
        draft: window.coursewareDraftResult,
        final: window.coursewareResult,
      }));

      await page.evaluate(({ draftKey, resultKey, data }) => {
        if (typeof data.draft !== 'undefined') {
          localStorage.setItem(draftKey, JSON.stringify(data.draft));
        }
        if (typeof data.final !== 'undefined') {
          localStorage.setItem(resultKey, JSON.stringify(data.final));
        }
      }, { draftKey: draftStorageKey, resultKey: resultStorageKey, data: payload });
    }
  `.trim();
}

async function pollResultIntoLocalStorage(
  playwrightCli,
  sessionName,
  pageTimeoutMs,
) {
  const runCode = buildPollResultRunCode();
  await runCli(
    playwrightCli,
    ["-s=" + sessionName, "run-code", runCode],
    pageTimeoutMs,
  );
}

function extractSerializedValue(localStorageOutput, storageKey) {
  const resultLine = String(localStorageOutput || "")
    .split("\n")
    .find((line) => line.startsWith(`${storageKey}=`));

  if (!resultLine) {
    return null;
  }
  return resultLine.slice(`${storageKey}=`.length);
}

// ---------------------------------------------------------------------------
// Result normalization
// ---------------------------------------------------------------------------

function extractPreviewUrl(raw) {
  if (!raw || typeof raw !== "object" || raw.success !== true) {
    return null;
  }
  const pieceId = String(raw.data?.pieceId || "").trim();
  if (!pieceId) {
    return null;
  }
  return `${COURSEWARE_PREVIEW_BASE_URL}?pieceId=${encodeURIComponent(pieceId)}`;
}

function normalizeResult(raw, draftRaw, topic) {
  const success = Boolean(
    raw && typeof raw === "object" && raw.success === true,
  );
  const previewUrl = extractPreviewUrl(raw);
  return {
    topic,
    success,
    data: success
      ? { ...(raw.data ?? {}), ...(previewUrl ? { previewUrl } : {}) }
      : null,
    error: success
      ? null
      : raw && typeof raw === "object"
        ? raw.error || "课件生成失败"
        : "课件生成失败",
    notice:
      success && previewUrl
        ? `课件生成成功，可通过 ${previewUrl} 预览课件。`
        : null,
    draft: normalizeDraftResult(draftRaw, topic),
    generatedAt: new Date().toISOString(),
    raw,
  };
}

function normalizeDraftResult(raw, topic) {
  const success = Boolean(
    raw && typeof raw === "object" && raw.success === true,
  );
  const previewUrl = extractPreviewUrl(raw);

  if (!success) {
    return null;
  }

  return {
    topic,
    success: true,
    data: { ...(raw.data ?? {}), ...(previewUrl ? { previewUrl } : {}) },
    notice: previewUrl
      ? `课件草稿已保存，可通过 ${previewUrl} 预览课件。`
      : null,
    generatedAt: new Date().toISOString(),
    raw,
  };
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

function deriveDraftOutputPath(outputPath) {
  const parsed = path.parse(outputPath);
  const ext = parsed.ext || ".json";
  return path.join(parsed.dir, `${parsed.name}.draft${ext}`);
}

// ---------------------------------------------------------------------------
// Task ID & XML helpers
// ---------------------------------------------------------------------------

function deriveTaskId(sessionName) {
  const suffix = sessionName.startsWith("seewo-claw-")
    ? sessionName.slice("seewo-claw-".length)
    : sessionName;
  return `task-${suffix}`;
}

function escapeXml(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function buildTaskXml({ taskId, coursewareName, pieceId }) {
  return [
    "<task>",
    "  <taskType>iframe</taskType>",
    `  <taskId>${escapeXml(taskId)}</taskId>`,
    "  <payload>",
    "    <iframeType>bloom-course</iframeType>",
    `    <coursewareName>${escapeXml(coursewareName)}</coursewareName>`,
    `    <pieceId>${escapeXml(pieceId)}</pieceId>`,
    "  </payload>",
    "</task>",
  ].join("\n");
}

function buildNoticeXml({ taskId }) {
  return [
    "<notice>",
    "  <noticeType>task-complete</noticeType>",
    `  <taskId>${escapeXml(taskId)}</taskId>`,
    "  <payload />",
    "</notice>",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

function printOutput(text) {
  process.stdout.write(text + "\n");
}

function printJson(payload) {
  process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
}

function fail(message, outputPath, topic) {
  printJson({
    ok: false,
    outputPath: outputPath || null,
    topic: topic || null,
    error: message,
  });
  process.exit(1);
}

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

module.exports = {
  // Constants
  COURSEWARE_PREVIEW_BASE_URL,
  DRAFT_STORAGE_KEY,
  RESULT_STORAGE_KEY,

  // Utilities
  parsePositiveInt,
  parseBooleanFlag,
  logStep,
  sleep,
  ensureDir,

  // Playwright CLI
  resolvePlaywrightCli,
  assertRuntimeDependencies,
  runCli,
  closeSessionQuietly,

  // Polling
  buildPollResultRunCode,
  pollResultIntoLocalStorage,
  extractSerializedValue,

  // Normalization
  extractPreviewUrl,
  normalizeResult,
  normalizeDraftResult,

  // Path
  deriveDraftOutputPath,

  // Task / XML
  deriveTaskId,
  escapeXml,
  buildTaskXml,
  buildNoticeXml,

  // Output
  printOutput,
  printJson,
  fail,
};
