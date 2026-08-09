import { it } from "@jest/globals";

const MULTI_PROCESS_TEST_TIMEOUT_MS = 10_000;

export function multiProcessTest(name: string, test: () => Promise<void>): void {
  it(name, test, MULTI_PROCESS_TEST_TIMEOUT_MS);
}
