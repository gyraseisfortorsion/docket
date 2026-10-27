#!/usr/bin/env node
'use strict';

const path = require('node:path');
const { spawnSync } = require('node:child_process');
const metadata = require('../package.json');
const platform = `${process.platform}-${process.arch}`;
const name = `${metadata.name}-${platform}`;
if (!['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64'].includes(platform)) {
  console.error(`Docket does not provide a prebuilt binary for ${platform}.`);
  process.exit(1);
}
let binary;
try {
  binary = path.join(path.dirname(require.resolve(`${name}/package.json`)), 'bin', 'docket');
} catch {
  console.error(`Docket's ${platform} package is missing. Reinstall without --omit=optional or --ignore-optional.`);
  process.exit(1);
}
const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  console.error(`Could not start Docket: ${result.error.message}`);
  process.exit(1);
}
if (result.signal) {
  // Match the native command's termination behavior.
  process.kill(process.pid, result.signal);
} else {
  process.exit(result.status === null ? 1 : result.status);
}
