import { it } from "@jest/globals";

const PROCESS_TEST_TIMEOUT_MS = 30_000;

// Keep the release-load budget scoped to tests that spawn the real Codex process.
export function processTest(name: string, test: () => Promise<void>): void {
  it(name, test, PROCESS_TEST_TIMEOUT_MS);
}
