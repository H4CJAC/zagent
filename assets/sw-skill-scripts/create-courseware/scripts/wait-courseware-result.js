#!/usr/bin/env node

const fs = require("fs");
const path = require("path");
const shared = require("./lib/shared");

// ---------------------------------------------------------------------------
// Environment variables
// ---------------------------------------------------------------------------

const SESSION_NAME = (process.env.SEEWO_CLAW_SESSION || "").trim();
const TASK_ID =
  (process.env.SEEWO_CLAW_TASK_ID || "").trim() ||
  (SESSION_NAME ? shared.deriveTaskId(SESSION_NAME) : "");
const COURSEWARE_TOPIC = (process.env.SEEWO_CLAW_TOPIC || "").trim();
const OUTPUT_PATH = path.resolve(
  process.env.SEEWO_CLAW_OUTPUT || "courseware-result.json",
);
const DRAFT_OUTPUT_PATH = path.resolve(
  process.env.SEEWO_CLAW_DRAFT_OUTPUT ||
    shared.deriveDraftOutputPath(OUTPUT_PATH),
);
const PAGE_TIMEOUT_MS = shared.parsePositiveInt(
  process.env.SEEWO_CLAW_PAGE_TIMEOUT_MS,
  15000,
);
const RESULT_TIMEOUT_MS = shared.parsePositiveInt(
  process.env.SEEWO_CLAW_RESULT_TIMEOUT_MS,
  900000,
);
const POLL_INTERVAL_MS = shared.parsePositiveInt(
  process.env.SEEWO_CLAW_POLL_INTERVAL_MS,
  2000,
);
const PLAYWRIGHT_CLI = shared.resolvePlaywrightCli(
  process.env.SEEWO_CLAW_PLAYWRIGHT_CLI || "playwright-cli",
);
const DEBUG_MODE = shared.parseBooleanFlag(process.env.SEEWO_CLAW_DEBUG);

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
  if (!SESSION_NAME) {
    fail("缺少环境变量 SEEWO_CLAW_SESSION。请传入 Script 1 输出的 sessionName。");
  }

  try {
    shared.assertRuntimeDependencies(PLAYWRIGHT_CLI);
    shared.logStep(`接力会话: ${SESSION_NAME}`);
    shared.logStep("开始等待 window.coursewareResult");

    const { draftResult, finalResult } = await waitForFinalResult();

    const normalized = shared.normalizeResult(
      finalResult,
      draftResult,
      COURSEWARE_TOPIC,
    );
    writeResultToFile(normalized);

    shared.printOutput(shared.buildNoticeXml({ taskId: TASK_ID }));

    shared.logStep(`TASK_ID=${TASK_ID}`);
    shared.logStep("课件生成完成");
  } catch (error) {
    fail(error instanceof Error ? error.message : String(error));
  } finally {
    await shared.closeSessionQuietly(
      PLAYWRIGHT_CLI,
      SESSION_NAME,
      PAGE_TIMEOUT_MS,
      DEBUG_MODE,
    );
  }
}

// ---------------------------------------------------------------------------
// Final result polling
// ---------------------------------------------------------------------------

async function waitForFinalResult() {
  const startedAt = Date.now();
  let latestDraftResult = null;

  while (Date.now() - startedAt < RESULT_TIMEOUT_MS) {
    await shared.pollResultIntoLocalStorage(
      PLAYWRIGHT_CLI,
      SESSION_NAME,
      PAGE_TIMEOUT_MS,
    );

    // 顺便读取草稿（用于写入最终 JSON 的 draft 字段）
    const draftStorageOutput = await shared.runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "localstorage-get", shared.DRAFT_STORAGE_KEY],
      PAGE_TIMEOUT_MS,
    );
    const serializedDraft = shared.extractSerializedValue(
      draftStorageOutput,
      shared.DRAFT_STORAGE_KEY,
    );
    if (serializedDraft && !latestDraftResult) {
      try {
        latestDraftResult = JSON.parse(serializedDraft);
      } catch (_) {
        // ignore parse error
      }
    }

    // 读取最终结果
    const resultStorageOutput = await shared.runCli(
      PLAYWRIGHT_CLI,
      ["-s=" + SESSION_NAME, "localstorage-get", shared.RESULT_STORAGE_KEY],
      PAGE_TIMEOUT_MS,
    );
    const serializedResult = shared.extractSerializedValue(
      resultStorageOutput,
      shared.RESULT_STORAGE_KEY,
    );
    if (serializedResult) {
      shared.logStep("已读取到课件最终结果");
      try {
        return {
          draftResult: latestDraftResult,
          finalResult: JSON.parse(serializedResult),
        };
      } catch (_) {
        throw new Error(
          `无法解析页面返回的课件结果: ${serializedResult.slice(0, 500)}`,
        );
      }
    }

    const elapsedSeconds = Math.floor((Date.now() - startedAt) / 1000);
    shared.logStep(`课件仍在生成中，已等待 ${elapsedSeconds} 秒`);
    await shared.sleep(POLL_INTERVAL_MS);
  }

  throw new Error(
    "等待 window.coursewareResult 超时。" +
      " 如果这是长任务，请增大 SEEWO_CLAW_RESULT_TIMEOUT_MS。",
  );
}

// ---------------------------------------------------------------------------
// File output
// ---------------------------------------------------------------------------

function writeResultToFile(result) {
  shared.ensureDir(path.dirname(OUTPUT_PATH));
  fs.writeFileSync(OUTPUT_PATH, JSON.stringify(result, null, 2));
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

main();
