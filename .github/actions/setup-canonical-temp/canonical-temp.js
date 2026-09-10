const fs = require("node:fs");
const path = require("node:path");

const DEFAULT_VOLUME_ROOT_PREFIX = "/Volumes";
const TEMP_PREFIX = "codexci";
const RANDOM_SUFFIX_BYTES = 6;
const MAX_TEMP_PATH_BYTES = 48;
const STATE_VERSION = 1;

const byteLength = (value) => Buffer.byteLength(value, "utf8");

function statIdentity(stat) {
  return { dev: String(stat.dev), ino: String(stat.ino) };
}

const sameIdentity = (left, right) =>
  left.dev === right.dev && left.ino === right.ino;

function isSafeVolumeName(volumeName) {
  return (
    /^[A-Za-z0-9._-]+$/.test(volumeName) &&
    volumeName !== "." &&
    volumeName !== ".."
  );
}

function isGeneratedTempName(name) {
  return /^codexci[A-Za-z0-9]{6}$/.test(name);
}

function layoutForRunnerTemp({
  runnerTemp,
  fsApi = fs,
  volumeRootPrefix = DEFAULT_VOLUME_ROOT_PREFIX,
  maxTempPathBytes = MAX_TEMP_PATH_BYTES,
}) {
  if (!runnerTemp || !path.isAbsolute(runnerTemp)) {
    throw new Error("RUNNER_TEMP must be an absolute path");
  }

  const canonicalVolumeRootPrefix = fsApi.realpathSync(volumeRootPrefix);
  if (canonicalVolumeRootPrefix !== volumeRootPrefix) {
    throw new Error("volume root prefix must not be a symlink");
  }

  const canonicalRunnerTemp = fsApi.realpathSync(runnerTemp);
  const relativeRunnerTemp = path.relative(
    canonicalVolumeRootPrefix,
    canonicalRunnerTemp,
  );
  const [volumeName, ...remainder] = relativeRunnerTemp.split(path.sep);
  if (!volumeName || remainder.length === 0 || !isSafeVolumeName(volumeName)) {
    throw new Error(
      "RUNNER_TEMP must be a directory below /Volumes/<safe-volume>",
    );
  }

  const volumeRootCandidate = path.join(canonicalVolumeRootPrefix, volumeName);
  const canonicalVolumeRoot = fsApi.realpathSync(volumeRootCandidate);
  if (canonicalVolumeRoot !== volumeRootCandidate) {
    throw new Error("external volume root must be canonical");
  }

  const mountParentStats = fsApi.statSync(canonicalVolumeRootPrefix, {
    bigint: true,
  });
  const volumeStats = fsApi.statSync(canonicalVolumeRoot, { bigint: true });
  const runnerTempStats = fsApi.statSync(canonicalRunnerTemp, { bigint: true });
  if (
    !mountParentStats.isDirectory() ||
    !volumeStats.isDirectory() ||
    !runnerTempStats.isDirectory()
  ) {
    throw new Error("volume root and RUNNER_TEMP must be directories");
  }
  if (String(volumeStats.dev) === String(mountParentStats.dev)) {
    throw new Error(
      "external volume root is not a separate mount from /Volumes",
    );
  }
  if (String(volumeStats.dev) !== String(runnerTempStats.dev)) {
    throw new Error("RUNNER_TEMP crosses an external-volume device boundary");
  }

  const tempPrefix = path.join(canonicalVolumeRoot, TEMP_PREFIX);
  const expectedPathBytes = byteLength(tempPrefix) + RANDOM_SUFFIX_BYTES;
  if (expectedPathBytes > maxTempPathBytes) {
    throw new Error(
      `canonical temp path exceeds ${maxTempPathBytes} UTF-8 bytes`,
    );
  }

  const writableDirectory = fsApi.constants.W_OK | fsApi.constants.X_OK;
  fsApi.accessSync(canonicalVolumeRoot, writableDirectory);
  fsApi.accessSync(canonicalRunnerTemp, writableDirectory);

  return {
    canonicalRunnerTemp,
    volumeRoot: canonicalVolumeRoot,
    volumeName,
    volumeIdentity: statIdentity(volumeStats),
    tempPrefix,
    maxTempPathBytes,
  };
}

function currentUid(value) {
  return typeof value === "number" ? value : process.getuid();
}

function allocateCanonicalTemp({
  env = process.env,
  fsApi = fs,
  platform = process.platform,
  volumeRootPrefix = DEFAULT_VOLUME_ROOT_PREFIX,
  maxTempPathBytes = MAX_TEMP_PATH_BYTES,
  uid,
}) {
  if (platform !== "darwin") {
    throw new Error("canonical local Rust CI temp requires macOS");
  }

  const layout = layoutForRunnerTemp({
    runnerTemp: env.RUNNER_TEMP,
    fsApi,
    volumeRootPrefix,
    maxTempPathBytes,
  });
  let tempPath;
  let allocated;
  try {
    tempPath = fsApi.mkdtempSync(layout.tempPrefix);
    const initialStat = fsApi.lstatSync(tempPath, { bigint: true });
    allocated = {
      identity: statIdentity(initialStat),
      uid: Number(initialStat.uid),
    };
    fsApi.chmodSync(tempPath, 0o700);
    const canonicalTempPath = fsApi.realpathSync(tempPath);
    if (
      canonicalTempPath !== tempPath ||
      byteLength(tempPath) > maxTempPathBytes ||
      path.dirname(tempPath) !== layout.volumeRoot ||
      !isGeneratedTempName(path.basename(tempPath))
    ) {
      throw new Error("mkdtemp did not return a short canonical path");
    }
    const stat = fsApi.lstatSync(tempPath, { bigint: true });
    if (
      !stat.isDirectory() ||
      Number(stat.mode) & 0o077 ||
      !sameIdentity(statIdentity(stat), allocated.identity)
    ) {
      throw new Error("canonical temp directory is not a private directory");
    }
    const ownerUid = currentUid(uid);
    if (allocated.uid !== ownerUid || Number(stat.uid) !== ownerUid) {
      throw new Error(
        "canonical temp directory owner changed during allocation",
      );
    }
    const state = {
      version: STATE_VERSION,
      path: tempPath,
      uid: ownerUid,
      identity: statIdentity(stat),
      volume: {
        root: layout.volumeRoot,
        name: layout.volumeName,
        identity: layout.volumeIdentity,
      },
    };
    return state;
  } catch (error) {
    if (tempPath && allocated) {
      try {
        const currentStat = fsApi.lstatSync(tempPath, { bigint: true });
        if (
          path.dirname(tempPath) === layout.volumeRoot &&
          isGeneratedTempName(path.basename(tempPath)) &&
          currentStat.isDirectory() &&
          !currentStat.isSymbolicLink() &&
          sameIdentity(statIdentity(currentStat), allocated.identity) &&
          Number(currentStat.uid) === allocated.uid &&
          fsApi.realpathSync(tempPath) === tempPath &&
          allocated.uid === currentUid(uid)
        ) {
          fsApi.rmSync(tempPath, { recursive: true, force: false });
        } else {
          error.message +=
            "; allocation cleanup refused after identity changed";
        }
      } catch (cleanupError) {
        error.message += `; allocation cleanup failed: ${cleanupError.message}`;
      }
    } else if (tempPath) {
      error.message += "; allocation cleanup refused without an owned identity";
    }
    throw error;
  }
}

function validateState(state) {
  if (
    !state ||
    state.version !== STATE_VERSION ||
    typeof state.path !== "string"
  ) {
    throw new Error("canonical temp state is invalid");
  }
  if (
    !Number.isInteger(state.uid) ||
    !state.identity ||
    typeof state.identity.dev !== "string" ||
    typeof state.identity.ino !== "string" ||
    !state.volume
  ) {
    throw new Error("canonical temp state is incomplete");
  }
  if (
    !path.isAbsolute(state.path) ||
    !state.volume.root ||
    !state.volume.name ||
    !state.volume.identity ||
    typeof state.volume.identity.dev !== "string" ||
    typeof state.volume.identity.ino !== "string"
  ) {
    throw new Error("canonical temp volume state is incomplete");
  }
  if (!isSafeVolumeName(state.volume.name)) {
    throw new Error("canonical temp volume name is unsafe");
  }
}

// Call only for owned, identity-checked roots after their writers stop. Open
// without following links and chmod the verified descriptor, never a link target.
function repairReadonlyDirectories(
  root,
  identity,
  uid,
  fsApi,
  { unlinkSymlinks = false } = {},
) {
  const pending = [root];
  while (pending.length) {
    const directory = pending.pop();
    let stat;
    try {
      stat = fsApi.lstatSync(directory, { bigint: true });
    } catch (error) {
      if (error.code === "ENOENT" && directory !== root) continue;
      throw error;
    }
    if (stat.isSymbolicLink() || !stat.isDirectory()) continue;
    if (
      String(stat.dev) !== identity.dev ||
      (directory === root && !sameIdentity(statIdentity(stat), identity)) ||
      Number(stat.uid) !== uid ||
      fsApi.realpathSync(directory) !== directory
    ) {
      throw new Error(
        `cleanup directory identity or owner changed: ${directory}`,
      );
    }
    const fd = fsApi.openSync(
      directory,
      fsApi.constants.O_RDONLY |
        fsApi.constants.O_NOFOLLOW |
        fsApi.constants.O_DIRECTORY,
    );
    try {
      const opened = fsApi.fstatSync(fd, { bigint: true });
      if (
        !opened.isDirectory() ||
        !sameIdentity(statIdentity(opened), statIdentity(stat)) ||
        Number(opened.uid) !== uid
      ) {
        throw new Error(
          `cleanup directory changed while opening: ${directory}`,
        );
      }
      const mode = Number(opened.mode) & 0o777;
      if ((mode & 0o700) !== 0o700) {
        try {
          fsApi.fchmodSync(fd, mode | 0o700);
        } catch (error) {
          error.message += `; directory ${directory}, mode ${mode.toString(8)}`;
          throw error;
        }
      }
    } finally {
      fsApi.closeSync(fd);
    }
    for (const entry of fsApi.readdirSync(directory, { withFileTypes: true })) {
      const child = path.join(directory, entry.name);
      if (entry.isSymbolicLink() && unlinkSymlinks) {
        // Runner.Sdk clears read-only attributes through symlinks. Remove the
        // owned link itself so deletion cannot make a shared SDK writable.
        // This traversal requires exclusive ownership and stopped writers.
        const link = fsApi.lstatSync(child, { bigint: true });
        const parent = fsApi.lstatSync(directory, { bigint: true });
        if (
          !link.isSymbolicLink() ||
          Number(link.uid) !== uid ||
          String(link.dev) !== identity.dev ||
          !sameIdentity(statIdentity(parent), statIdentity(stat)) ||
          !parent.isDirectory() ||
          Number(parent.uid) !== uid ||
          fsApi.realpathSync(directory) !== directory
        ) {
          throw new Error(
            `cleanup symlink identity or owner changed: ${child}`,
          );
        }
        fsApi.unlinkSync(child);
      } else if (!entry.isFile() && !entry.isSymbolicLink()) {
        // Avoid one lstat per regular build output; only directories need
        // permission repair. Unknown entry types still get checked above.
        pending.push(child);
      }
    }
  }
}

function cleanupCanonicalTemp({
  state,
  fsApi = fs,
  platform = process.platform,
  env = process.env,
  volumeRootPrefix = DEFAULT_VOLUME_ROOT_PREFIX,
  maxTempPathBytes = MAX_TEMP_PATH_BYTES,
  uid,
  randomSuffix = `${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`,
}) {
  validateState(state);
  if (platform !== "darwin") {
    throw new Error("canonical local Rust CI temp requires macOS");
  }

  let layout;
  try {
    layout = layoutForRunnerTemp({
      runnerTemp: env.RUNNER_TEMP,
      fsApi,
      volumeRootPrefix,
      maxTempPathBytes,
    });
  } catch (error) {
    throw new Error(`refusing canonical temp cleanup: ${error.message}`);
  }

  const expectedVolumeIdentity = state.volume.identity;
  if (
    layout.volumeRoot !== state.volume.root ||
    layout.volumeName !== state.volume.name ||
    !sameIdentity(layout.volumeIdentity, expectedVolumeIdentity)
  ) {
    throw new Error("refusing canonical temp cleanup: volume identity changed");
  }

  const expectedPath = state.path;
  if (
    path.dirname(expectedPath) !== layout.volumeRoot ||
    !isGeneratedTempName(path.basename(expectedPath)) ||
    byteLength(expectedPath) > layout.maxTempPathBytes
  ) {
    throw new Error(
      "refusing canonical temp cleanup: path is outside the generated layout",
    );
  }

  try {
    const stat = fsApi.lstatSync(expectedPath, { bigint: true });
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      throw new Error("path is no longer a canonical directory");
    }
    if (
      !sameIdentity(statIdentity(stat), state.identity) ||
      Number(stat.uid) !== state.uid ||
      Number(stat.uid) !== currentUid(uid)
    ) {
      throw new Error("path identity or owner changed");
    }
    if (fsApi.realpathSync(expectedPath) !== expectedPath) {
      throw new Error("path is no longer canonical");
    }
  } catch (error) {
    if (error && error.code === "ENOENT") {
      return { status: "already-clean" };
    }
    throw new Error(`refusing canonical temp cleanup: ${error.message}`);
  }

  let quarantine;
  try {
    quarantine = fsApi.mkdtempSync(
      path.join(layout.volumeRoot, `codexcleanup-${randomSuffix}-`),
    );
    const quarantineStat = fsApi.lstatSync(quarantine, { bigint: true });
    if (
      quarantineStat.isSymbolicLink() ||
      !quarantineStat.isDirectory() ||
      Number(quarantineStat.uid) !== currentUid(uid)
    ) {
      throw new Error("cleanup quarantine identity changed");
    }
    const tombstone = path.join(quarantine, path.basename(expectedPath));
    try {
      fsApi.lstatSync(tombstone, { bigint: true });
      throw new Error("cleanup quarantine destination already exists");
    } catch (error) {
      if (error && error.code !== "ENOENT") {
        throw error;
      }
    }
    fsApi.renameSync(expectedPath, tombstone);
    const renamed = fsApi.lstatSync(tombstone, { bigint: true });
    if (
      renamed.isSymbolicLink() ||
      !renamed.isDirectory() ||
      !sameIdentity(statIdentity(renamed), state.identity) ||
      Number(renamed.uid) !== state.uid ||
      fsApi.realpathSync(tombstone) !== tombstone
    ) {
      throw new Error("renamed path identity changed");
    }
    for (let attempt = 0; ; attempt++) {
      const current = fsApi.lstatSync(quarantine, { bigint: true });
      if (
        !current.isDirectory() ||
        current.isSymbolicLink() ||
        !sameIdentity(statIdentity(current), statIdentity(quarantineStat)) ||
        Number(current.uid) !== state.uid ||
        fsApi.realpathSync(quarantine) !== quarantine
      ) {
        throw new Error("cleanup quarantine identity changed");
      }
      try {
        fsApi.rmSync(quarantine, { recursive: true, force: false });
        break;
      } catch (error) {
        if (
          attempt >= 2 ||
          !["EACCES", "EPERM", "ENOTEMPTY"].includes(error.code)
        ) {
          throw error;
        }
        try {
          repairReadonlyDirectories(
            quarantine,
            statIdentity(quarantineStat),
            state.uid,
            fsApi,
          );
        } catch (repairError) {
          repairError.message += `; while removing: ${error.code}: ${error.message}`;
          throw repairError;
        }
      }
    }
    return { status: "cleaned" };
  } catch (error) {
    throw new Error(
      `canonical temp cleanup failed; retained ${quarantine || expectedPath}: ${error.message}`,
    );
  }
}

module.exports = {
  MAX_TEMP_PATH_BYTES,
  TEMP_PREFIX,
  allocateCanonicalTemp,
  cleanupCanonicalTemp,
  layoutForRunnerTemp,
  repairReadonlyDirectories,
};
