import { execFile } from "node:child_process";
import path from "node:path";
import { promisify } from "node:util";

import { expect, it } from "@jest/globals";

import {
  assistantMessage,
  customToolCall,
  responseCompleted,
  responseStarted,
  sse,
  startResponsesTestProxy,
} from "./responsesProxy";
import { codexExecPath, createTestClient } from "./testCodex";

const execFileAsync = promisify(execFile);
const HOST_MARKER = "staged sibling host launched";
const runHostSmoke = process.env.CODEX_SDK_CODE_MODE_HOST_SMOKE === "1" ? it : it.skip;

type CustomToolCallOutput = {
  type?: string;
  call_id?: string;
  output?: unknown;
};

async function stagedHostProcesses(): Promise<string[]> {
  const hostPath = path.join(path.dirname(codexExecPath), "codex-code-mode-host");
  const { stdout } = await execFileAsync("/bin/ps", ["-axo", "command="]);
  return stdout
    .split("\n")
    .map((command) => command.trim())
    .filter((command) => command === hostPath || command.startsWith(`${hostPath} `));
}

runHostSmoke(
  "launches and cleans up the staged sibling code-mode host",
  async () => {
    expect(await stagedHostProcesses()).toEqual([]);
    const { url, close, requests } = await startResponsesTestProxy({
      responseBodies: [
        sse(
          responseStarted("host-smoke-1"),
          customToolCall("host-smoke-call", "exec", `text(${JSON.stringify(HOST_MARKER)})`),
          responseCompleted("host-smoke-1"),
        ),
        sse(
          responseStarted("host-smoke-2"),
          assistantMessage("host smoke complete"),
          responseCompleted("host-smoke-2"),
        ),
      ],
    });
    const { client, cleanup } = createTestClient({
      baseUrl: url,
      config: {
        features: {
          code_mode_only: true,
          code_mode_host: {
            enabled: true,
            disable_in_process_fallback: true,
          },
        },
      },
    });

    try {
      const result = await client.startThread().run("exercise the staged sibling host");

      expect(result.finalResponse).toBe("host smoke complete");
      expect(requests).toHaveLength(2);
      const followUpInput = requests[1]!.json.input as unknown as CustomToolCallOutput[];
      const hostOutput = followUpInput.find(
        (item) => item.type === "custom_tool_call_output" && item.call_id === "host-smoke-call",
      );
      expect(JSON.stringify(hostOutput?.output)).toContain(HOST_MARKER);
    } finally {
      cleanup();
      await close();
    }

    expect(await stagedHostProcesses()).toEqual([]);
  },
  30_000,
);
