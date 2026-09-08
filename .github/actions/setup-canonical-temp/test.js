const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");
const test = require("node:test");

const {
  MAX_TEMP_PATH_BYTES,
  allocateCanonicalTemp,
  cleanupCanonicalTemp,
  layoutForRunnerTemp,
} = require("./canonical-temp.js");
const { run: runMain } = require("./main.js");
const { run: runPost } = require("./post.js");

function withStatProperty(stat, name, value) {
  const copy = Object.assign(Object.create(Object.getPrototypeOf(stat)), stat);
  return Object.defineProperty(copy, name, { value, enumerable: true });
}

function fixture() {
  const rawRoot = fs.mkdtempSync("/tmp/canon-");
  const root = fs.realpathSync(rawRoot);
  const volumeRootPrefix = path.join(root, "Volumes");
  const volumeRoot = path.join(volumeRootPrefix, "Developer-Artifacts");
  const runnerTemp = path.join(volumeRoot, "runner", "_temp");
  fs.mkdirSync(runnerTemp, { recursive: true });
  const fsApi = {
    ...fs,
    statSync(value, options) {
      const stat = fs.statSync(value, options);
      const device =
        value === volumeRootPrefix
          ? 1
          : value === volumeRoot || value.startsWith(`${volumeRoot}${path.sep}`)
            ? 7
            : undefined;
      return device === undefined
        ? stat
        : withStatProperty(
            stat,
            "dev",
            options?.bigint ? BigInt(device) : device,
          );
    },
  };
  return { root, volumeRootPrefix, volumeRoot, runnerTemp, fsApi };
}

function fixtureOptions(paths, env = {}) {
  return {
    env: { RUNNER_TEMP: paths.runnerTemp, ...env },
    platform: "darwin",
    volumeRootPrefix: paths.volumeRootPrefix,
    fsApi: paths.fsApi,
    maxTempPathBytes: 4096,
    uid: process.getuid(),
  };
}

async function withFixture(callback) {
  const paths = fixture();
  try {
    return await callback(paths);
  } finally {
    fs.rmSync(paths.root, { recursive: true, force: true });
  }
}

function assertRefusesCleanup(state, options) {
  assert.throws(() => cleanupCanonicalTemp({ state, ...options }), /refusing/);
}

function assertCleanupStatus(state, options, status) {
  assert.equal(cleanupCanonicalTemp({ state, ...options }).status, status);
}

function fakeVolumeFs({ device = 7, realpath = (value) => value } = {}) {
  return {
    constants: fs.constants,
    realpathSync: realpath,
    statSync(value) {
      return {
        dev: value === "/Volumes" ? 1 : device,
        ino: value.length,
        uid: 501,
        isDirectory: () => true,
      };
    },
    accessSync() {},
  };
}

async function listenOnSocket(socketPath) {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, () => {
      server.removeListener("error", reject);
      resolve();
    });
  });
  await new Promise((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
}

test("allocates canonical private directories with isolated socket paths", async () => {
  await withFixture(async (paths) => {
    const options = fixtureOptions(paths);
    const first = allocateCanonicalTemp(options);
    const second = allocateCanonicalTemp(options);
    assert.notEqual(first.path, second.path);
    assert.equal(fs.realpathSync(first.path), first.path);
    assert.equal(fs.realpathSync(second.path), second.path);
    assert.equal(fs.statSync(first.path).mode & 0o777, 0o700);
    assert.equal(fs.statSync(second.path).mode & 0o777, 0o700);
    await listenOnSocket(path.join(first.path, "s"));
    await listenOnSocket(path.join(second.path, "s"));
    assertCleanupStatus(first, options, "cleaned");
    assertCleanupStatus(second, options, "cleaned");
  });
});

test("setup-ci keeps read-only DotSlash caches outside canonical cleanup", () => {
  return withFixture((paths) => {
    const options = fixtureOptions(paths);
    const state = allocateCanonicalTemp(options);
    const envFile = path.join(paths.root, "github-env");
    const action = fs.readFileSync(
      path.join(__dirname, "../setup-ci/action.yml"),
      "utf8",
    );
    const configure = action
      .split("    - name: Configure CI build paths\n")[1]
      .split("      run: |\n")[1]
      .split("\n    - name:")[0]
      .replace(/^ {8}/gm, "");
    const result = spawnSync("bash", ["-c", configure], {
      encoding: "utf8",
      env: {
        PATH: process.env.PATH,
        RUNNER_ENVIRONMENT: "self-hosted",
        RUNNER_NAME_VALUE: "cleanup-test",
        RUNNER_OS_NAME: "macOS",
        RUNNER_OS: "macOS",
        RUNNER_TEMP: paths.runnerTemp,
        CI_BUILD_ROOT: path.join(paths.runnerTemp, "build"),
        CANONICAL_TEMP_DIR: state.path,
        CODEX_LOCAL_RUST_CI: "true",
        GITHUB_ENV: envFile,
        GITHUB_OUTPUT: path.join(paths.root, "github-output"),
        GITHUB_WORKSPACE: paths.root,
      },
    });
    assert.equal(result.status, 0, result.stderr);
    const exported = Object.fromEntries(
      fs
        .readFileSync(envFile, "utf8")
        .trim()
        .split("\n")
        .map((line) => {
          const separator = line.indexOf("=");
          return [line.slice(0, separator), line.slice(separator + 1)];
        }),
    );
    assert.equal(
      exported.DOTSLASH_CACHE,
      path.join(paths.runnerTemp, "dotslashcache"),
    );
    assert.equal(exported.TMPDIR, state.path);
    assert.equal(
      exported.SCCACHE_SERVER_UDS,
      path.join(state.path, "sccache.sock"),
    );
    const immutableCache = path.join(exported.DOTSLASH_CACHE, "tool");
    fs.mkdirSync(immutableCache);
    const tool = path.join(immutableCache, "binary");
    fs.writeFileSync(tool, "owned test fixture");
    fs.chmodSync(immutableCache, 0o555);
    try {
      assertCleanupStatus(state, options, "cleaned");
      assert.equal(fs.readFileSync(tool, "utf8"), "owned test fixture");
      assert.equal(fs.statSync(immutableCache).mode & 0o777, 0o555);
    } finally {
      fs.chmodSync(immutableCache, 0o755);
    }
  });
});

test("main and post exchange the GitHub state key and clean the allocated directory", () => {
  return withFixture((paths) => {
    const outputFile = path.join(paths.root, "output");
    const stateFile = path.join(paths.root, "state");
    const options = fixtureOptions(paths, {
      GITHUB_OUTPUT: outputFile,
      GITHUB_STATE: stateFile,
    });
    assert.throws(
      () => runMain({ ...options, env: { RUNNER_TEMP: paths.runnerTemp } }),
      /GITHUB_OUTPUT and GITHUB_STATE are required/,
    );
    assert.deepEqual(fs.readdirSync(paths.volumeRoot), ["runner"]);
    runMain(options);
    const output = fs.readFileSync(outputFile, "utf8");
    const stateLine = fs.readFileSync(stateFile, "utf8").trim();
    const state = JSON.parse(stateLine.slice("CANONICAL_TEMP=".length));
    assert.equal(output.trim(), `temp-dir=${state.path}`);
    runPost({
      ...options,
      env: { ...options.env, STATE_CANONICAL_TEMP: JSON.stringify(state) },
    });
    assert.equal(fs.existsSync(state.path), false);
  });
});

test("cleanup refuses a symlink replacement, a different inode, an owner change, and a path change", () => {
  return withFixture((paths) => {
    const options = fixtureOptions(paths);

    const symlinkState = allocateCanonicalTemp(options);
    const symlinkTarget = path.join(paths.root, "symlink-target");
    fs.mkdirSync(symlinkTarget);
    fs.rmSync(symlinkState.path, { recursive: true });
    fs.symlinkSync(symlinkTarget, symlinkState.path);
    assertRefusesCleanup(symlinkState, options);
    assert.equal(fs.lstatSync(symlinkState.path).isSymbolicLink(), true);
    fs.rmSync(symlinkState.path);
    fs.rmSync(symlinkTarget, { recursive: true });

    const inodeState = allocateCanonicalTemp(options);
    const originalInodePath = `${inodeState.path}-original`;
    fs.renameSync(inodeState.path, originalInodePath);
    fs.mkdirSync(inodeState.path);
    assertRefusesCleanup(inodeState, options);
    fs.rmSync(inodeState.path, { recursive: true });
    fs.rmSync(originalInodePath, { recursive: true });

    const missingState = allocateCanonicalTemp(options);
    fs.rmSync(missingState.path, { recursive: true });
    assertCleanupStatus(missingState, options, "already-clean");

    const ownerState = allocateCanonicalTemp(options);
    const ownerFs = {
      ...paths.fsApi,
      lstatSync(value, options) {
        const stat = fs.lstatSync(value, options);
        return value === ownerState.path
          ? withStatProperty(stat, "uid", ownerState.uid + 1)
          : stat;
      },
    };
    assertRefusesCleanup(ownerState, { ...options, fsApi: ownerFs });
    assert.equal(fs.existsSync(ownerState.path), true);
    cleanupCanonicalTemp({ state: ownerState, ...options });

    const first = allocateCanonicalTemp(options);
    const second = allocateCanonicalTemp(options);
    assertRefusesCleanup({ ...first, path: second.path }, options);
    cleanupCanonicalTemp({ state: first, ...options });
    cleanupCanonicalTemp({ state: second, ...options });

    const retainedState = allocateCanonicalTemp(options);
    fs.writeFileSync(path.join(retainedState.path, "marker"), "preserved");
    let quarantinePath;
    const afterRenameFs = {
      ...paths.fsApi,
      mkdtempSync(prefix) {
        quarantinePath = fs.mkdtempSync(prefix);
        return quarantinePath;
      },
      lstatSync(value, lstatOptions) {
        const stat = fs.lstatSync(value, lstatOptions);
        const tombstone = path.join(
          quarantinePath ?? "/",
          path.basename(retainedState.path),
        );
        return value === tombstone
          ? withStatProperty(stat, "ino", stat.ino + 1n)
          : stat;
      },
    };
    assert.throws(
      () =>
        cleanupCanonicalTemp({
          state: retainedState,
          ...options,
          fsApi: afterRenameFs,
          randomSuffix: "after-rename",
        }),
      (error) => {
        assert.ok(error.message.includes(`retained ${quarantinePath}:`));
        return true;
      },
    );
    const retainedDir = path.join(
      quarantinePath,
      path.basename(retainedState.path),
    );
    assert.deepEqual(fs.readdirSync(retainedDir), ["marker"]);
    assert.equal(
      fs.readFileSync(path.join(retainedDir, "marker"), "utf8"),
      "preserved",
    );
  });
});

test("fails closed for boot-disk, unsafe, symlinked, cross-device, and overlong layouts", () => {
  const common = {
    runnerTemp: "/Volumes/Developer-Artifacts/runner/_temp",
    fsApi: fakeVolumeFs(),
  };
  assert.throws(
    () => layoutForRunnerTemp({ ...common, runnerTemp: "/private/tmp/runner" }),
    /below/,
  );
  assert.throws(
    () =>
      layoutForRunnerTemp({
        ...common,
        runnerTemp: "/Volumes/Developer Artifacts/runner/_temp",
      }),
    /safe-volume/,
  );
  assert.throws(
    () =>
      layoutForRunnerTemp({
        ...common,
        fsApi: fakeVolumeFs({
          realpath: (value) =>
            value === "/Volumes/Developer-Artifacts" ? "/Volumes/real" : value,
        }),
      }),
    /canonical/,
  );
  assert.throws(
    () =>
      layoutForRunnerTemp({
        ...common,
        fsApi: {
          ...fakeVolumeFs(),
          statSync(value) {
            const stat = fakeVolumeFs().statSync(value);
            return value.endsWith("_temp") ? { ...stat, dev: 8 } : stat;
          },
        },
      }),
    /device boundary/,
  );
  assert.throws(
    () =>
      layoutForRunnerTemp({ ...common, fsApi: fakeVolumeFs({ device: 1 }) }),
    /separate mount/,
  );
  const longVolume = `${"v".repeat(40)}`;
  assert.throws(
    () =>
      layoutForRunnerTemp({
        runnerTemp: `/Volumes/${longVolume}/runner/_temp`,
        fsApi: fakeVolumeFs(),
      }),
    new RegExp(`${MAX_TEMP_PATH_BYTES} UTF-8 bytes`),
  );
  const layout = layoutForRunnerTemp({ ...common });
  assert.equal(Buffer.byteLength(layout.tempPrefix) + 6, 42);
});
