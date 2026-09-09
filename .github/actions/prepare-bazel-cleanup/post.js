const { prepare } = require("./cleanup.js");
const { runAction } = require("../setup-canonical-temp/run-action.js");

runAction(() => {
  if (!process.env.STATE_BAZEL_CLEANUP) return;
  const state = JSON.parse(process.env.STATE_BAZEL_CLEANUP);
  console.log(`Bazel runner cleanup: ${prepare({ state })}`);
});
