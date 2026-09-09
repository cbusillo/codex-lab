const fs = require("node:fs");
const { capture } = require("./cleanup.js");
const { runAction } = require("../setup-canonical-temp/run-action.js");

runAction(() => {
  fs.appendFileSync(
    process.env.GITHUB_STATE,
    `BAZEL_CLEANUP=${JSON.stringify(capture())}\n`,
    "utf8",
  );
});
