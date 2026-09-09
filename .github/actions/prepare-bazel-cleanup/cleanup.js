const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const {
  layoutForRunnerTemp,
  repairReadonlyDirectories,
} = require("../setup-canonical-temp/canonical-temp.js");

function directoryIdentity(directory, uid) {
  const stat = fs.lstatSync(directory, { bigint: true });
  if (
    !stat.isDirectory() ||
    Number(stat.uid) !== uid ||
    fs.realpathSync(directory) !== directory
  ) {
    throw new Error(
      `Bazel cleanup requires an owned canonical directory: ${directory}`,
    );
  }
  return { path: directory, dev: String(stat.dev), ino: String(stat.ino) };
}

function capture({
  env = process.env,
  platform = process.platform,
  layoutForTemp = layoutForRunnerTemp,
} = {}) {
  if (platform !== "darwin")
    throw new Error("Bazel cleanup preparation requires macOS");
  const uid = process.getuid();
  const layout = layoutForTemp({ runnerTemp: env.RUNNER_TEMP });
  const buildRoot = path.join(layout.canonicalRunnerTemp, "codex-ci");
  const expected = {
    CI_BUILD_ROOT: buildRoot,
    BAZEL_OUTPUT_BASE: path.join(buildRoot, "o"),
    BAZEL_OUTPUT_USER_ROOT: path.join(buildRoot, "b"),
    BAZEL_REPO_CONTENTS_CACHE: path.join(
      layout.canonicalRunnerTemp,
      "bazel-repo-contents-cache",
    ),
  };
  for (const [key, value] of Object.entries(expected)) {
    if (env[key] !== value)
      throw new Error(`Unexpected ${key} for Bazel cleanup`);
  }
  const roots = Object.fromEntries(
    Object.entries(expected).map(([key, directory]) => [
      key,
      directoryIdentity(directory, uid),
    ]),
  );
  if (
    Object.values(roots).some((root) => root.dev !== layout.volumeIdentity.dev)
  ) {
    throw new Error("Bazel cleanup root crosses the runner volume boundary");
  }
  return {
    version: 1,
    workspace: fs.realpathSync(env.GITHUB_WORKSPACE),
    uid,
    job: [env.GITHUB_RUN_ID, env.GITHUB_RUN_ATTEMPT, env.GITHUB_JOB],
    volume: layout.volumeIdentity,
    runnerTemp: directoryIdentity(layout.canonicalRunnerTemp, uid),
    roots,
  };
}

function prepare({ state, env = process.env, spawn = spawnSync, ...options }) {
  const validate = () => {
    if (
      JSON.stringify(capture({ env, ...options })) !== JSON.stringify(state)
    ) {
      throw new Error(
        "Bazel cleanup job, directory, or volume identity changed",
      );
    }
  };
  validate();
  const shim = path.join(env.CI_BUILD_ROOT, "bin", "bazel");
  let shimStat;
  try {
    shimStat = fs.lstatSync(shim);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (shimStat) {
    if (
      !shimStat.isFile() ||
      shimStat.uid !== state.uid ||
      fs.realpathSync(shim) !== shim
    ) {
      throw new Error("Bazel cleanup shim is not an owned canonical file");
    }
    // Bazel 9 waits for server termination, including startup-option changes:
    // src/main/cpp/blaze.cc KillRunningServer and Communicate. A timeout or
    // lock conflict fails here before any directory permissions are changed.
    const shutdown = spawn(
      shim,
      [
        "--ignore_all_rc_files",
        "--noblock_for_lock",
        `--output_base=${env.BAZEL_OUTPUT_BASE}`,
        `--output_user_root=${env.BAZEL_OUTPUT_USER_ROOT}`,
        "--noexperimental_remote_repo_contents_cache",
        "shutdown",
      ],
      {
        env,
        cwd: state.workspace,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 1024 * 1024,
      },
    );
    if (shutdown.error || shutdown.status !== 0) {
      throw new Error(
        `Bazel shutdown failed; permissions unchanged: ${shutdown.error?.message || shutdown.stderr || shutdown.status}`,
      );
    }
  } else if (fs.readdirSync(env.BAZEL_OUTPUT_BASE).length !== 0) {
    throw new Error(
      "Bazel cleanup cannot establish shutdown without its job shim",
    );
  }
  validate();
  // The server is stopped and the runner still owns deletion of these job roots.
  // Repair parents first: Runner.Sdk's parallel deletion can otherwise suppress
  // permission errors for their children and finally report ENOTEMPTY.
  for (const root of [
    state.roots.CI_BUILD_ROOT,
    state.roots.BAZEL_REPO_CONTENTS_CACHE,
  ]) {
    repairReadonlyDirectories(root.path, root, state.uid, fs);
  }
  return "prepared";
}

module.exports = { capture, prepare };
