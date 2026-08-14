import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { afterEach, beforeEach } from "@jest/globals";

const originalCodexLabHome = process.env.CODEX_LAB_HOME;
const originalCodexHome = process.env.CODEX_HOME;
let currentCodexLabHome: string | undefined;

beforeEach(async () => {
  currentCodexLabHome = await fs.mkdtemp(path.join(os.tmpdir(), "codex-sdk-test-"));
  process.env.CODEX_LAB_HOME = currentCodexLabHome;
  process.env.CODEX_HOME = currentCodexLabHome;
});

afterEach(async () => {
  const codexLabHomeToDelete = currentCodexLabHome;
  currentCodexLabHome = undefined;

  if (originalCodexLabHome === undefined) {
    delete process.env.CODEX_LAB_HOME;
  } else {
    process.env.CODEX_LAB_HOME = originalCodexLabHome;
  }
  if (originalCodexHome === undefined) {
    delete process.env.CODEX_HOME;
  } else {
    process.env.CODEX_HOME = originalCodexHome;
  }

  if (codexLabHomeToDelete) {
    await fs.rm(codexLabHomeToDelete, { recursive: true, force: true });
  }
});
