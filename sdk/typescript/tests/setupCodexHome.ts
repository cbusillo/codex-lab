import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { afterEach, beforeEach } from "@jest/globals";

const originalCodexLabHome = process.env.CODEX_LAB_HOME;
let currentCodexHome: string | undefined;

beforeEach(async () => {
  currentCodexHome = await fs.mkdtemp(path.join(os.tmpdir(), "codex-sdk-test-"));
  process.env.CODEX_LAB_HOME = currentCodexHome;
});

afterEach(async () => {
  const codexHomeToDelete = currentCodexHome;
  currentCodexHome = undefined;

  if (originalCodexLabHome === undefined) {
    delete process.env.CODEX_LAB_HOME;
  } else {
    process.env.CODEX_LAB_HOME = originalCodexLabHome;
  }

  if (codexHomeToDelete) {
    await fs.rm(codexHomeToDelete, { recursive: true, force: true });
  }
});
