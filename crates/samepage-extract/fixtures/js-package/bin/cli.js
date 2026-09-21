const { spawn } = require("child_process");

function launchWorker() {
  const child = spawn("node", ["worker.js"]);
  return child;
}

module.exports = { launchWorker };
