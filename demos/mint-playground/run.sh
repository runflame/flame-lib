#!/usr/bin/env bash
set -euo pipefail

APP="$(cd "$(dirname "$0")" && pwd)"

if ! command -v npm >/dev/null; then
  echo "The frontend is built with npm: install Node.js (https://nodejs.org) and run this again." >&2
  exit 1
fi

# The frontend, into web/dist. React, Vite and the browser wallet come from npm.
cd "$APP/web"
if [ ! -d node_modules ] || [ package-lock.json -nt node_modules ]; then
  npm ci --no-audit --no-fund
fi
npm run build

cd "$APP"
exec cargo run --locked --manifest-path "$APP/Cargo.toml" -- "$@"
