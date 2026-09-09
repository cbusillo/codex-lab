const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { capture, prepare } = require("./cleanup.js");

function fixture(t) {
  const temp = fs.realpathSync(
    fs.mkdtempSync(path.join(os.tmpdir(), "bazel-cleanup-")),
  );
  const env = {
    RUNNER_TEMP: temp,
    GITHUB_WORKSPACE: temp,
    CI_BUILD_ROOT: path.join(temp, "codex-ci"),
    BAZEL_OUTPUT_BASE: path.join(temp, "codex-ci/o"),
    BAZEL_OUTPUT_USER_ROOT: path.join(temp, "codex-ci/b"),
    BAZEL_REPO_CONTENTS_CACHE: path.join(temp, "bazel-repo-contents-cache"),
    GITHUB_RUN_ID: "123",
    GITHUB_RUN_ATTEMPT: "1",
    GITHUB_JOB: "bazel",
  };
  for (const key of [
    "BAZEL_OUTPUT_BASE",
    "BAZEL_OUTPUT_USER_ROOT",
    "BAZEL_REPO_CONTENTS_CACHE",
  ]) {
    fs.mkdirSync(env[key], { recursive: true });
  }
  const volumeIdentity = { dev: String(fs.statSync(temp).dev), ino: "1" };
  const options = {
    env,
    platform: "darwin",
    layoutForTemp: () => ({
      canonicalRunnerTemp: temp,
      volumeIdentity,
      volumeRoot: temp,
      volumeName: path.basename(temp),
      tempPrefix: path.join(temp, "codexci"),
      maxTempPathBytes: 48,
    }),
  };
  const state = JSON.parse(JSON.stringify(capture(options)));
  const readonly = path.join(env.BAZEL_OUTPUT_BASE, "python/licenses");
  fs.mkdirSync(readonly, { recursive: true });
  const file = path.join(readonly, "LICENSE");
  fs.writeFileSync(file, "fixture");
  fs.chmodSync(file, 0o444);
  fs.chmodSync(readonly, 0o555);
  t.after(() => {
    if (fs.existsSync(readonly)) fs.chmodSync(readonly, 0o755);
    fs.rmSync(temp, { recursive: true, force: true });
  });
  fs.mkdirSync(path.join(env.CI_BUILD_ROOT, "bin"));
  const shim = path.join(env.CI_BUILD_ROOT, "bin/bazel");
  fs.writeFileSync(shim, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  return { ...options, state, readonly, file, shim, temp };
}

const mode = (file) => fs.statSync(file).mode & 0o777;

test("shutdown precedes directory-only repair, without following symlinks", (t) => {
  const f = fixture(t);
  const outside = path.join(f.temp, "outside");
  fs.mkdirSync(outside, { mode: 0o555 });
  fs.symlinkSync(
    outside,
    path.join(f.env.BAZEL_REPO_CONTENTS_CACHE, "outside"),
  );
  t.after(() => {
    if (fs.existsSync(outside)) fs.chmodSync(outside, 0o755);
  });
  const cached = path.join(f.env.BAZEL_REPO_CONTENTS_CACHE, "readonly-cache");
  fs.mkdirSync(cached, { mode: 0o555 });
  let calls = 0;
  const result = prepare({
    ...f,
    spawn: (command, args, options) => {
      calls++;
      assert.equal(command, f.shim);
      assert.equal(mode(f.readonly), 0o555);
      assert.deepEqual(args, [
        "--ignore_all_rc_files",
        "--noblock_for_lock",
        `--output_base=${f.env.BAZEL_OUTPUT_BASE}`,
        `--output_user_root=${f.env.BAZEL_OUTPUT_USER_ROOT}`,
        "--noexperimental_remote_repo_contents_cache",
        "shutdown",
      ]);
      assert.equal(options.timeout, 60000);
      assert.equal(options.cwd, f.env.GITHUB_WORKSPACE);
      assert.equal(options.env, f.env);
      return { status: 0 };
    },
  });
  assert.equal(result, "prepared");
  assert.equal(calls, 1);
  assert.equal(mode(f.readonly), 0o755);
  assert.equal(mode(f.file), 0o444);
  assert.equal(mode(cached), 0o755);
  assert.equal(mode(outside), 0o555);
  assert.equal(
    fs.existsSync(path.join(f.env.BAZEL_REPO_CONTENTS_CACHE, "outside")),
    false,
  );
});

for (const failure of [
  { status: 9, stderr: "lock held" },
  { status: null, error: new Error("timeout") },
]) {
  test(`failed shutdown leaves permissions unchanged: ${failure.stderr || "timeout"}`, (t) => {
    const f = fixture(t);
    const link = path.join(f.env.BAZEL_OUTPUT_BASE, "sdk-link");
    fs.symlinkSync(f.temp, link);
    assert.throws(
      () => prepare({ ...f, spawn: () => failure }),
      /shutdown failed/,
    );
    assert.equal(mode(f.readonly), 0o555);
    assert.equal(fs.lstatSync(link).isSymbolicLink(), true);
  });
}

for (const changed of [
  "job",
  "owner",
  "volume",
  "directory",
  "symlink",
  "routing",
]) {
  test(`refuses changed ${changed} before shutdown`, (t) => {
    const f = fixture(t);
    if (changed === "job") f.env.GITHUB_RUN_ATTEMPT = "2";
    if (changed === "owner") f.state.uid++;
    if (changed === "volume") f.state.volume.ino = "changed";
    if (changed === "routing") f.env.BAZEL_OUTPUT_BASE = f.temp;
    if (changed === "directory" || changed === "symlink") {
      fs.renameSync(
        f.env.BAZEL_REPO_CONTENTS_CACHE,
        `${f.env.BAZEL_REPO_CONTENTS_CACHE}-old`,
      );
      if (changed === "directory")
        fs.mkdirSync(f.env.BAZEL_REPO_CONTENTS_CACHE);
      else
        fs.symlinkSync(
          `${f.env.BAZEL_REPO_CONTENTS_CACHE}-old`,
          f.env.BAZEL_REPO_CONTENTS_CACHE,
        );
    }
    assert.throws(() =>
      prepare({ ...f, spawn: () => assert.fail("must not run") }),
    );
    assert.equal(mode(f.readonly), 0o555);
  });
}

test("revalidates roots after shutdown", (t) => {
  const f = fixture(t);
  assert.throws(
    () =>
      prepare({
        ...f,
        spawn: () => {
          f.env.GITHUB_JOB = "other";
          return { status: 0 };
        },
      }),
    /identity changed/,
  );
  assert.equal(mode(f.readonly), 0o555);
});

test("missing shim refuses nonempty output but allows setup failure before Bazel starts", (t) => {
  const f = fixture(t);
  fs.unlinkSync(f.shim);
  assert.throws(() => prepare(f), /without its job shim/);
  assert.equal(mode(f.readonly), 0o555);
  fs.chmodSync(f.readonly, 0o755);
  fs.rmSync(path.join(f.env.BAZEL_OUTPUT_BASE, "python"), { recursive: true });
  assert.equal(
    prepare({ ...f, spawn: () => assert.fail("must not run") }),
    "prepared",
  );
});
