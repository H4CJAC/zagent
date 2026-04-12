#!/usr/bin/env node

const fs = require("fs");
const path = require("path");
const shared = require("./lib/shared");

// ---------------------------------------------------------------------------
// Environment variables
// ---------------------------------------------------------------------------

// const TARGET_URL =
//   process.env.SEEWO_CLAW_URL ||
//   "https://bloom2.test.seewo.com/ai-workspace/seewo-claw/generate-courseware";

const TARGET_URL =
  process.env.SEEWO_CLAW_URL ||
  "https://bloom-inner.seewo.com/ai-workspace/seewo-claw/generate-courseware";

const COURSEWARE_TOPIC = (process.env.SEEWO_CLAW_TOPIC || "").trim();
const OUTPUT_PATH = path.resolve(
  process.env.SEEWO_CLAW_OUTPUT || "courseware-result.json",
);
const DRAFT_OUTPUT_PATH = path.resolve(
  process.env.SEEWO_CLAW_DRAFT_OUTPUT ||
    shared.deriveDraftOutputPath(OUTPUT_PATH),
);
const COURSEWARE_CONTEXTS = parseContextInputs(
  process.env.SEEWO_CLAW_CONTEXT_FILE,
  process.env.SEEWO_CLAW_CONTEXT_FILE_DESCRIPTION,
  process.env.SEEWO_CLAW_CONTEXT_JSON,
  process.env.SEEWO_CLAW_CONTEXT_TEXT,
  process.env.SEEWO_CLAW_CONTEXT_TEXT_DESCRIPTION,
);
const SESSION_NAME =
  process.env.SEEWO_CLAW_SESSION ||
  `seewo-claw-${Math.random().toString(36).slice(2, 8)}`;
const PAGE_TIMEOUT_MS = shared.parsePositiveInt(
  process.env.SEEWO_CLAW_PAGE_TIMEOUT_MS,
  15000,
);
const DRAFT_TIMEOUT_MS = shared.parsePositiveInt(
  process.env.SEEWO_CLAW_DRAFT_TIMEOUT_MS,
  180000,
);
const POLL_INTERVAL_MS = shared.parsePositiveInt(
  process.env.SEEWO_CLAW_POLL_INTERVAL_MS,
  2000,
);
const PLAYWRIGHT_CLI = shared.resolvePlaywrightCli(
  process.env.SEEWO_CLAW_PLAYWRIGHT_CLI || "playwright-cli",
);
const DEBUG_MODE = shared.parseBooleanFlag(process.env.SEEWO_CLAW_DEBUG);
const COOKIE_DEFINITIONS = buildCookieDefinitions(
  process.env.SEEWO_CLAW_COOKIE_JSON,
  process.env.SEEWO_CLAW_X_TOKEN,
  TARGET_URL,
);
const PAGE_STATE_MARKER = "SEEWO_CLAW_PAGE_STATE=";

// ---------------------------------------------------------------------------
// Local fail shorthand
// ---------------------------------------------------------------------------

function fail(message) {
  shared.fail(message, OUTPUT_PATH, COURSEWARE_TOPIC);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  if (!COURSEWARE_TOPIC) {
    fail("缺少环境变量 SEEWO_CLAW_TOPIC。");
  }

  try {
    shared.assertRuntimeDependencies(PLAYWRIGHT_CLI);
    shared.logStep("初始化浏览器会话");
    await shared.runCli(PLAYWRIGHT_CLI, buildOpenArgs(), PAGE_TIMEOUT_MS);
    await applyCookiesIfNeeded();
    shared.logStep(`进入目标页面: ${TARGET_URL}`);
    await shared.runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "goto", TARGET_URL],
      PAGE_TIMEOUT_MS,
    );
    shared.logStep(`开始创建课件，主题: ${COURSEWARE_TOPIC}`);
    await startCoursewareGeneration();
    const rawDraftResult = await waitForDraftResult();
    const normalized = shared.normalizeDraftResult(
      rawDraftResult,
      COURSEWARE_TOPIC,
    );
    writeDraftResultToFile(normalized);

    const pieceId = String(rawDraftResult?.data?.pieceId || "").trim();
    const taskId = shared.deriveTaskId(SESSION_NAME);

    shared.printOutput(
      shared.buildTaskXml({
        taskId,
        coursewareName: COURSEWARE_TOPIC,
        pieceId,
      }),
    );

    shared.logStep(`SESSION_NAME=${SESSION_NAME}`);
    shared.logStep(`TASK_ID=${taskId}`);
  } catch (error) {
    await shared.closeSessionQuietly(
      PLAYWRIGHT_CLI,
      SESSION_NAME,
      PAGE_TIMEOUT_MS,
      DEBUG_MODE,
    );
    fail(error instanceof Error ? error.message : String(error));
  }
  // 成功时不关闭会话，留给 wait-courseware-result.js 接力
}

// ---------------------------------------------------------------------------
// Courseware generation (inject topic + click button)
// ---------------------------------------------------------------------------

async function startCoursewareGeneration() {
  await injectTopicAndResetResult();
  const createButtonRef = await findCreateButtonRef();
  shared.logStep(`已定位"创建课件"按钮: ref=${createButtonRef}`);
  await shared.runCli(
    PLAYWRIGHT_CLI,
    ["-s=" + SESSION_NAME, "click", createButtonRef],
    PAGE_TIMEOUT_MS,
  );
  shared.logStep(
    `已执行"创建课件"按钮点击，开始等待 window.coursewareDraftResult`,
  );
}

async function injectTopicAndResetResult() {
  const runCode = buildInjectTopicRunCode();
  await shared.runCli(
    PLAYWRIGHT_CLI,
    ["-s=" + SESSION_NAME, "run-code", runCode],
    PAGE_TIMEOUT_MS,
  );
}

async function findCreateButtonRef() {
  const waitRunCode = buildWaitForCreateButtonRunCode();
  shared.logStep(`等待"创建课件"按钮渲染完成`);
  await shared
    .runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "run-code", waitRunCode],
      PAGE_TIMEOUT_MS,
    )
    .catch(async (error) => {
      throw new Error(
        `等待"创建课件"按钮失败。${await buildPageStateDiagnosticMessage()} 原始错误: ${error.message || String(error)}`,
      );
    });
  const snapshotOutput = await shared.runCli(
    PLAYWRIGHT_CLI,
    ["-s=" + SESSION_NAME, "snapshot"],
    PAGE_TIMEOUT_MS,
  );
  const snapshotContent = loadSnapshotContent(snapshotOutput);
  if (!snapshotContent) {
    throw new Error(
      `未能从 snapshot 输出中解析快照文件路径。${await buildPageStateDiagnosticMessage()} 命令输出预览: ${snapshotOutput.slice(0, 500)}`,
    );
  }
  const buttonRef = extractCreateButtonRef(snapshotContent);
  if (!buttonRef) {
    throw new Error(
      `未在页面快照中定位到"创建课件"按钮。${await buildPageStateDiagnosticMessage()} 快照内容预览: ${snapshotContent.slice(0, 500)}`,
    );
  }
  return buttonRef;
}

// ---------------------------------------------------------------------------
// Draft result polling (Phase 1 only waits for draft)
// ---------------------------------------------------------------------------

async function waitForDraftResult() {
  const startedAt = Date.now();

  while (Date.now() - startedAt < DRAFT_TIMEOUT_MS) {
    await shared.pollResultIntoLocalStorage(
      PLAYWRIGHT_CLI,
      SESSION_NAME,
      PAGE_TIMEOUT_MS,
    );

    const draftStorageOutput = await shared.runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "localstorage-get", shared.DRAFT_STORAGE_KEY],
      PAGE_TIMEOUT_MS,
    );
    const serializedDraftResult = shared.extractSerializedValue(
      draftStorageOutput,
      shared.DRAFT_STORAGE_KEY,
    );
    if (serializedDraftResult) {
      const parsed = JSON.parse(serializedDraftResult);
      const pieceId = String(parsed?.data?.pieceId || "").trim();
      const previewUrl = shared.extractPreviewUrl(parsed);
      shared.logStep(
        `已拿到首次保存结果${pieceId ? `，pieceId=${pieceId}` : ""}${previewUrl ? `，previewUrl=${previewUrl}` : ""}`,
      );
      return parsed;
    }

    const elapsedSeconds = Math.floor((Date.now() - startedAt) / 1000);
    shared.logStep(`等待草稿结果中，已等待 ${elapsedSeconds} 秒`);
    await shared.sleep(POLL_INTERVAL_MS);
  }

  const debugState = await readResultDebugState();
  throw new Error(
    "等待 window.coursewareDraftResult 超时。" +
      " 如果这是长任务，请增大 SEEWO_CLAW_DRAFT_TIMEOUT_MS。" +
      " 页面状态: " +
      JSON.stringify(debugState),
  );
}

// ---------------------------------------------------------------------------
// File output
// ---------------------------------------------------------------------------

function writeDraftResultToFile(result) {
  if (!result) {
    return;
  }
  shared.ensureDir(path.dirname(DRAFT_OUTPUT_PATH));
  fs.writeFileSync(DRAFT_OUTPUT_PATH, JSON.stringify(result, null, 2));
}

// ---------------------------------------------------------------------------
// Browser session helpers
// ---------------------------------------------------------------------------

function buildOpenArgs() {
  const args = ["-s=" + SESSION_NAME, "open"];
  if (DEBUG_MODE) {
    args.push("--headed");
  }
  args.push("about:blank");
  return args;
}

async function applyCookiesIfNeeded() {
  if (COOKIE_DEFINITIONS.length === 0) {
    return;
  }
  shared.logStep(`开始预注入 ${COOKIE_DEFINITIONS.length} 个 cookie`);
  for (const cookie of COOKIE_DEFINITIONS) {
    await shared.runCli(
      PLAYWRIGHT_CLI,
      buildCookieSetArgs(cookie),
      PAGE_TIMEOUT_MS,
    );
  }
  shared.logStep("cookie 预注入完成");
}

function buildCookieSetArgs(cookie) {
  const args = ["-s=" + SESSION_NAME, "cookie-set", cookie.name, cookie.value];
  if (cookie.domain) args.push("--domain", cookie.domain);
  if (cookie.path) args.push("--path", cookie.path);
  if (typeof cookie.expires === "number")
    args.push("--expires", String(cookie.expires));
  if (typeof cookie.httpOnly === "boolean")
    args.push("--httpOnly", String(cookie.httpOnly));
  if (typeof cookie.secure === "boolean")
    args.push("--secure", String(cookie.secure));
  if (cookie.sameSite) args.push("--sameSite", cookie.sameSite);
  return args;
}

// ---------------------------------------------------------------------------
// Page state & diagnostics
// ---------------------------------------------------------------------------

async function readResultDebugState() {
  const runCode = buildReadDebugStateRunCode();
  const output = await shared
    .runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "run-code", runCode],
      PAGE_TIMEOUT_MS,
    )
    .catch(() => "");
  const stateLine = String(output || "")
    .split("\n")
    .find((line) => line.startsWith("SEEWO_CLAW_DEBUG_STATE="));
  if (!stateLine) {
    return { note: "未能读取调试状态" };
  }
  try {
    return JSON.parse(stateLine.slice("SEEWO_CLAW_DEBUG_STATE=".length));
  } catch (_) {
    return { note: "调试状态解析失败", raw: stateLine };
  }
}

async function readCurrentPageState() {
  const diagnostics = [];

  const runCodeOutput = await shared
    .runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "run-code", buildReadPageStateRunCode()],
      PAGE_TIMEOUT_MS,
    )
    .catch((error) => {
      diagnostics.push(`run-code失败: ${error.message || String(error)}`);
      return "";
    });

  const pageStateFromRunCode = extractPageState(runCodeOutput);
  if (hasUsefulPageState(pageStateFromRunCode)) {
    return attachDiagnostics(pageStateFromRunCode, diagnostics);
  }
  if (runCodeOutput) {
    diagnostics.push(`run-code输出: ${compactText(runCodeOutput)}`);
  }

  const evalOutput = await shared
    .runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "eval", buildReadPageStateEvalExpression()],
      PAGE_TIMEOUT_MS,
    )
    .catch((error) => {
      diagnostics.push(`eval失败: ${error.message || String(error)}`);
      return "";
    });

  const pageStateFromEval = extractEvalPageState(evalOutput);
  if (hasUsefulPageState(pageStateFromEval)) {
    return attachDiagnostics(pageStateFromEval, diagnostics);
  }
  if (evalOutput) {
    diagnostics.push(`eval输出: ${compactText(evalOutput)}`);
  }

  const snapshotState = await readPageStateFromSnapshot(diagnostics);
  if (hasUsefulPageState(snapshotState)) {
    return attachDiagnostics(snapshotState, diagnostics);
  }

  return attachDiagnostics(
    { href: "", title: "", bodyTextPreview: "" },
    diagnostics,
  );
}

async function buildPageStateDiagnosticMessage() {
  const pageState = await readCurrentPageState();
  const actualPath = extractComparablePath(pageState.href);
  const expectedPath = extractComparablePath(TARGET_URL);
  const loginHint =
    actualPath && expectedPath && !actualPath.startsWith(expectedPath)
      ? looksLikeLoginRedirect(pageState)
        ? " 疑似缺少登录态并被重定向到登录页。"
        : " 页面可能发生了非预期跳转。"
      : "";

  return (
    `页面状态: 实际地址: ${pageState.href || "未知"}。` +
    ` 页面标题: ${pageState.title || "空"}。` +
    ` 页面内容摘要: ${pageState.bodyTextPreview || "空"}。` +
    (pageState.diagnostics ? ` 诊断信息: ${pageState.diagnostics}。` : "") +
    loginHint
  );
}

async function readPageStateFromSnapshot(diagnostics) {
  const snapshotOutput = await shared
    .runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "snapshot"],
      PAGE_TIMEOUT_MS,
    )
    .catch((error) => {
      diagnostics.push(`snapshot失败: ${error.message || String(error)}`);
      return "";
    });

  if (!snapshotOutput) {
    return { href: "", title: "", bodyTextPreview: "" };
  }

  const snapshotContent = loadSnapshotContent(snapshotOutput);
  if (!snapshotContent) {
    diagnostics.push(`snapshot输出: ${compactText(snapshotOutput)}`);
    return {
      href: "",
      title: "",
      bodyTextPreview: compactText(snapshotOutput).slice(0, 500),
    };
  }
  return {
    href: extractUrlFromText(snapshotContent),
    title: extractTitleFromSnapshot(snapshotContent),
    bodyTextPreview: compactText(snapshotContent).slice(0, 500),
  };
}

// ---------------------------------------------------------------------------
// Run-code builders
// ---------------------------------------------------------------------------

function buildInjectTopicRunCode() {
  return `
    async (page) => {
      const topic = ${JSON.stringify(COURSEWARE_TOPIC)};
      const contexts = ${JSON.stringify(COURSEWARE_CONTEXTS)};
      const draftStorageKey = ${JSON.stringify(shared.DRAFT_STORAGE_KEY)};
      const resultStorageKey = ${JSON.stringify(shared.RESULT_STORAGE_KEY)};
      await page.evaluate(({ injectedTopic, injectedContexts }) => {
        window.coursewareTopic = injectedTopic;
        window.coursewareContext = injectedContexts;
        window.coursewareDraftResult = undefined;
        window.coursewareResult = undefined;
      }, { injectedTopic: topic, injectedContexts: contexts });
      await page.evaluate(({ draftKey, resultKey }) => {
        localStorage.removeItem(draftKey);
        localStorage.removeItem(resultKey);
      }, { draftKey: draftStorageKey, resultKey: resultStorageKey });
    }
  `.trim();
}

function buildWaitForCreateButtonRunCode() {
  return `
    async (page) => {
      const createButton = page.getByRole('button', { name: '创建课件' });
      await createButton.waitFor({ state: 'visible', timeout: ${PAGE_TIMEOUT_MS} });
    }
  `.trim();
}

function buildReadPageStateRunCode() {
  return `
    async (page) => {
      const pageState = await page.evaluate(() => ({
        href: location.href,
        title: document.title || "",
        bodyTextPreview: ((document.body && document.body.innerText) || "").slice(0, 500)
      }));
      console.log(${JSON.stringify(PAGE_STATE_MARKER)} + JSON.stringify(pageState));
    }
  `.trim();
}

function buildReadPageStateEvalExpression() {
  return `JSON.stringify({
    href: location.href,
    title: document.title || "",
    bodyTextPreview: ((document.body && document.body.innerText) || "").slice(0, 500)
  })`;
}

function buildReadDebugStateRunCode() {
  return `
    async (page) => {
      const debugState = await page.evaluate(() => {
        const payload = window.coursewareResult;
        const draftPayload = window.coursewareDraftResult;
        let payloadPreview = '';
        let draftPayloadPreview = '';
        try {
          payloadPreview = JSON.stringify(payload).slice(0, 500);
        } catch (previewError) {
          payloadPreview = String(payload).slice(0, 500);
        }
        try {
          draftPayloadPreview = JSON.stringify(draftPayload).slice(0, 500);
        } catch (previewError) {
          draftPayloadPreview = String(draftPayload).slice(0, 500);
        }
        return {
          href: location.href,
          title: document.title || '',
          draftPayloadType: draftPayload === null ? 'null' : Array.isArray(draftPayload) ? 'array' : typeof draftPayload,
          draftPayloadPreview,
          payloadType: payload === null ? 'null' : Array.isArray(payload) ? 'array' : typeof payload,
          payloadPreview,
          bodyTextPreview: ((document.body && document.body.innerText) || '').slice(0, 500)
        };
      });
      console.log('SEEWO_CLAW_DEBUG_STATE=' + JSON.stringify(debugState));
    }
  `.trim();
}

// ---------------------------------------------------------------------------
// Snapshot & page state parsing helpers
// ---------------------------------------------------------------------------

function extractCreateButtonRef(snapshotOutput) {
  const lines = String(snapshotOutput || "").split("\n");
  for (const line of lines) {
    if (line.includes("创建课件")) {
      const match = line.match(/\[ref=([^\]]+)\]/);
      if (match) return match[1];
    }
  }
  return null;
}

function extractSnapshotPath(snapshotOutput) {
  const match = String(snapshotOutput || "").match(/\[Snapshot\]\(([^)]+)\)/);
  return match ? match[1] : null;
}

function extractInlineSnapshotContent(snapshotOutput) {
  const text = String(snapshotOutput || "");
  const fencedMatch = text.match(
    /### Snapshot\s*```(?:yaml|yml)?\s*([\s\S]*?)```/i,
  );
  if (fencedMatch && fencedMatch[1].trim()) {
    return fencedMatch[1].trim();
  }
  const headingIndex = text.search(/### Snapshot/i);
  if (headingIndex === -1) return "";
  const trailingContent = text
    .slice(headingIndex)
    .replace(/^### Snapshot\s*/i, "");
  return trailingContent.trim();
}

function loadSnapshotContent(snapshotOutput) {
  const snapshotPath = extractSnapshotPath(snapshotOutput);
  if (snapshotPath) {
    return fs.readFileSync(resolveCliArtifactPath(snapshotPath), "utf8");
  }
  return extractInlineSnapshotContent(snapshotOutput);
}

function resolveCliArtifactPath(snapshotPath) {
  return path.resolve(process.cwd(), snapshotPath);
}

function extractPageState(output) {
  const stateLine = String(output || "")
    .split("\n")
    .find((line) => line.startsWith(PAGE_STATE_MARKER));
  if (!stateLine) return { href: "", title: "", bodyTextPreview: "" };
  try {
    return JSON.parse(stateLine.slice(PAGE_STATE_MARKER.length));
  } catch (_) {
    return { href: "", title: "", bodyTextPreview: String(stateLine).slice(0, 500) };
  }
}

function extractEvalPageState(output) {
  const normalizedOutput = compactText(output);
  if (!normalizedOutput) return { href: "", title: "", bodyTextPreview: "" };
  const jsonCandidate = findJsonObject(normalizedOutput);
  if (!jsonCandidate) return extractPageStateFromLooseText(normalizedOutput);
  try {
    return JSON.parse(jsonCandidate);
  } catch (_) {
    const normalizedJsonCandidate = tryUnescapeJsonCandidate(jsonCandidate);
    if (normalizedJsonCandidate) {
      try {
        return JSON.parse(normalizedJsonCandidate);
      } catch (__) {
        // fall through
      }
    }
    return extractPageStateFromLooseText(jsonCandidate);
  }
}

function extractComparablePath(rawUrl) {
  if (!rawUrl) return "";
  try {
    return new URL(rawUrl).pathname.replace(/\/+$/, "");
  } catch (_) {
    return String(rawUrl).trim();
  }
}

function hasUsefulPageState(pageState) {
  return Boolean(
    pageState &&
      (pageState.href || pageState.title || pageState.bodyTextPreview),
  );
}

function attachDiagnostics(pageState, diagnostics) {
  return {
    href: pageState.href || "",
    title: pageState.title || "",
    bodyTextPreview: pageState.bodyTextPreview || "",
    diagnostics: diagnostics.length ? diagnostics.join(" | ") : "",
  };
}

function extractUrlFromText(text) {
  const match = String(text || "").match(/https?:\/\/[^\s)\]]+/);
  return match ? match[0] : "";
}

function extractPageStateFromLooseText(text) {
  const rawText = String(text || "");
  const normalizedText = rawText.replace(/\\"/g, '"');
  const hrefMatch =
    normalizedText.match(/"href"\s*:\s*"([^"]+)"/) ||
    normalizedText.match(/Page URL:\s*([^\s]+)/i);
  const titleMatch = normalizedText.match(/"title"\s*:\s*"([^"]*)"/);
  const bodyMatch = normalizedText.match(
    /"bodyTextPreview"\s*:\s*"([\s\S]*?)"/,
  );
  return {
    href: hrefMatch ? hrefMatch[1] : extractUrlFromText(normalizedText),
    title: titleMatch ? titleMatch[1] : "",
    bodyTextPreview: bodyMatch
      ? bodyMatch[1].replace(/\\n/g, "\n").slice(0, 500)
      : compactText(normalizedText).slice(0, 500),
  };
}

function extractTitleFromSnapshot(snapshotContent) {
  const lines = String(snapshotContent || "")
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  return lines[0] ? lines[0].slice(0, 200) : "";
}

function compactText(text) {
  return String(text || "")
    .replace(/\s+/g, " ")
    .trim();
}

function findJsonObject(text) {
  const start = text.indexOf("{");
  const end = text.lastIndexOf("}");
  if (start === -1 || end === -1 || end <= start) return "";
  return text.slice(start, end + 1);
}

function tryUnescapeJsonCandidate(text) {
  const rawText = String(text || "").trim();
  if (!rawText.includes('\\"')) return "";
  return rawText.replace(/\\"/g, '"').replace(/\\\\n/g, "\\n");
}

function looksLikeLoginRedirect(pageState) {
  const combined = [
    pageState.href || "",
    pageState.title || "",
    pageState.bodyTextPreview || "",
  ]
    .join("\n")
    .toLowerCase();
  const loginSignals = [
    "login", "signin", "sign-in", "auth", "passport", "sso", "cas",
    "登录", "请登录", "统一认证", "身份认证",
    "id.seewo.com", "id.test.seewo.com",
  ];
  return loginSignals.some((signal) => combined.includes(signal));
}

// ---------------------------------------------------------------------------
// Cookie parsing
// ---------------------------------------------------------------------------

function buildCookieDefinitions(rawJsonValue, rawXTokenValue, targetUrl) {
  const cookies = parseCookieDefinitions(rawJsonValue);
  const xTokenCookie = buildXTokenCookieDefinition(rawXTokenValue, targetUrl);
  if (!xTokenCookie) return cookies;
  const filteredCookies = cookies.filter((cookie) => cookie.name !== "x-token");
  filteredCookies.push(xTokenCookie);
  return filteredCookies;
}

function parseCookieDefinitions(rawValue) {
  if (!rawValue) return [];
  let parsed;
  try {
    parsed = JSON.parse(rawValue);
  } catch (_) {
    fail("SEEWO_CLAW_COOKIE_JSON 不是合法 JSON。");
  }
  if (!Array.isArray(parsed)) {
    fail("SEEWO_CLAW_COOKIE_JSON 必须是 cookie 数组。");
  }
  return parsed.map((item, index) => normalizeCookieDefinition(item, index));
}

function buildXTokenCookieDefinition(rawXTokenValue, targetUrl) {
  const tokenValue = String(rawXTokenValue || "").trim();
  if (!tokenValue) return null;
  return {
    name: "x-token",
    value: tokenValue,
    domain: buildCookieDomainFromUrl(targetUrl),
    path: "/",
    httpOnly: true,
    secure: true,
    sameSite: "None",
  };
}

function buildCookieDomainFromUrl(rawUrl) {
  try {
    const hostname = new URL(rawUrl).hostname;
    if (hostname.endsWith(".seewo.com")) return ".seewo.com";
    return hostname;
  } catch (_) {
    return ".seewo.com";
  }
}

function normalizeCookieDefinition(cookie, index) {
  if (!cookie || typeof cookie !== "object" || Array.isArray(cookie)) {
    fail(`SEEWO_CLAW_COOKIE_JSON 中第 ${index + 1} 个 cookie 不是对象。`);
  }
  const name = String(cookie.name || "").trim();
  if (!name) {
    fail(`SEEWO_CLAW_COOKIE_JSON 中第 ${index + 1} 个 cookie 缺少 name。`);
  }
  return {
    name,
    value: String(cookie.value || ""),
    domain: cookie.domain ? String(cookie.domain) : undefined,
    path: cookie.path ? String(cookie.path) : undefined,
    expires:
      typeof cookie.expires === "number" && Number.isFinite(cookie.expires)
        ? cookie.expires
        : undefined,
    httpOnly:
      typeof cookie.httpOnly === "boolean" ? cookie.httpOnly : undefined,
    secure: typeof cookie.secure === "boolean" ? cookie.secure : undefined,
    sameSite: cookie.sameSite ? String(cookie.sameSite) : undefined,
  };
}

// ---------------------------------------------------------------------------
// Context parsing
// ---------------------------------------------------------------------------

function parseContextInputs(
  rawFilePath,
  rawFileDescription,
  rawJsonValue,
  rawTextValue,
  rawTextDescription,
) {
  const contexts = [];
  contexts.push(
    ...parseContextDefinitionsFromFile(rawFilePath, rawFileDescription),
  );
  contexts.push(...parseContextDefinitionsFromJson(rawJsonValue));
  contexts.push(
    ...parseContextDefinitionsFromText(rawTextValue, rawTextDescription),
  );
  return contexts;
}

function parseContextDefinitionsFromFile(rawFilePath, rawFileDescription) {
  const filePath = String(rawFilePath || "").trim();
  if (!filePath) return [];
  const resolvedPath = path.resolve(filePath);
  let fileContent;
  try {
    fileContent = fs.readFileSync(resolvedPath, "utf8");
  } catch (_) {
    fail(`无法读取 SEEWO_CLAW_CONTEXT_FILE 指向的文件: ${resolvedPath}`);
  }
  try {
    return parseContextDefinitionsFromJson(
      fileContent,
      `SEEWO_CLAW_CONTEXT_FILE(${resolvedPath})`,
    );
  } catch (_) {
    return parseContextDefinitionsFromText(
      fileContent,
      rawFileDescription,
      `SEEWO_CLAW_CONTEXT_FILE(${resolvedPath})`,
    );
  }
}

function parseContextDefinitionsFromJson(
  rawValue,
  sourceName = "SEEWO_CLAW_CONTEXT_JSON",
) {
  if (!rawValue) return [];
  let parsed;
  try {
    parsed = JSON.parse(rawValue);
  } catch (_) {
    throw new Error(`${sourceName} 不是合法 JSON。`);
  }
  const contexts = Array.isArray(parsed) ? parsed : [parsed];
  return contexts.map((item, index) =>
    normalizeContextDefinition(item, index, sourceName),
  );
}

function parseContextDefinitionsFromText(
  rawTextValue,
  rawTextDescription,
  sourceName = "SEEWO_CLAW_CONTEXT_TEXT",
) {
  const textValue = String(rawTextValue || "").trim();
  if (!textValue) return [];
  return [
    {
      description:
        String(rawTextDescription || "").trim() ||
        (sourceName.includes("CONTEXT_FILE") ? "文件上下文" : "补充上下文"),
      value: textValue,
    },
  ];
}

function normalizeContextDefinition(context, index, sourceName) {
  if (!context || typeof context !== "object" || Array.isArray(context)) {
    fail(`${sourceName} 中第 ${index + 1} 个 context 不是对象。`);
  }
  const description = String(context.description || "").trim();
  if (!description) {
    fail(`${sourceName} 中第 ${index + 1} 个 context 缺少 description。`);
  }
  if (!Object.prototype.hasOwnProperty.call(context, "value")) {
    fail(`${sourceName} 中第 ${index + 1} 个 context 缺少 value。`);
  }
  return { description, value: context.value };
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

main();
