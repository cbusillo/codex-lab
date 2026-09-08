const { cleanupCanonicalTemp } = require("./canonical-temp.js");
const { runAction } = require("./run-action.js");

function run({ env = process.env, ...options } = {}) {
  const rawState = env.STATE_CANONICAL_TEMP;
  if (!rawState) {
    return;
  }
  const state = JSON.parse(rawState);
  console.log(
    `Canonical local Rust CI temp cleanup: ${cleanupCanonicalTemp({ state, env, ...options }).status}`,
  );
}

if (require.main === module) runAction(run);

module.exports = { run };
