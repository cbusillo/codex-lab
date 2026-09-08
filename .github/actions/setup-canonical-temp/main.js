const fs = require("node:fs");
const {
  allocateCanonicalTemp,
  cleanupCanonicalTemp,
} = require("./canonical-temp.js");
const { runAction } = require("./run-action.js");

function appendFileEnv(env, name, line) {
  fs.appendFileSync(env[name], `${line}\n`, "utf8");
}

function run({ env = process.env, ...options } = {}) {
  if (!env.GITHUB_OUTPUT || !env.GITHUB_STATE) {
    throw new Error(
      "GITHUB_OUTPUT and GITHUB_STATE are required before allocation",
    );
  }
  let state;
  try {
    state = allocateCanonicalTemp({ env, ...options });
    appendFileEnv(
      env,
      "GITHUB_STATE",
      `CANONICAL_TEMP=${JSON.stringify(state)}`,
    );
    appendFileEnv(env, "GITHUB_OUTPUT", `temp-dir=${state.path}`);
    console.log(`Using canonical local Rust CI temp directory: ${state.path}`);
  } catch (error) {
    if (state) {
      try {
        cleanupCanonicalTemp({ state, env, ...options });
      } catch (cleanupError) {
        error.message += `; allocation cleanup failed: ${cleanupError.message}`;
      }
    }
    throw error;
  }
}

if (require.main === module) runAction(run);

module.exports = { run };
