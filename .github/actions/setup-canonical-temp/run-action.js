function runAction(run) {
  try {
    run();
  } catch (error) {
    console.error(`::error::${error.message}`);
    process.exitCode = 1;
  }
}
module.exports = { runAction };
