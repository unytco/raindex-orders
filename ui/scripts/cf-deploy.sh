#!/usr/bin/env bash
#
# Cloudflare Workers Builds "Deploy command" for the hot-bridge-ui worker.
# Point the project's Deploy command at this (`npm run cf:deploy`); the Build
# command stays `npm run build`. Runs from the same dir as the build (ui/).
#
# Why: the faucet route reads FAUCET_PRIVATE_KEY / SEPOLIA_RPC_URL at runtime via
# $env/dynamic/private — i.e. from the Worker's runtime secret bindings, not the
# build. `wrangler deploy` resets dashboard-set bindings on every push, so runtime
# values added by hand get wiped. Workers Builds *build variables*, by contrast,
# persist. So we keep both values as build variables and re-apply them as runtime
# secrets here on every deploy: the wipe heals itself and nothing needs managing in
# the CF dashboard beyond pointing the Deploy command at this script once.
set -euo pipefail

: "${FAUCET_PRIVATE_KEY:?set FAUCET_PRIVATE_KEY as a Workers Builds build variable}"
: "${SEPOLIA_RPC_URL:?set SEPOLIA_RPC_URL as a Workers Builds build variable}"

# Ship the built worker. --keep-vars stops the deploy from dropping bindings not
# declared in wrangler.jsonc, so any secrets already present survive with no gap.
npx wrangler deploy --keep-vars

# Re-apply the runtime secrets from the build vars (worker name comes from
# wrangler.jsonc). Built with node for safe JSON escaping, written to a 0600 temp
# file, removed on exit, never echoed.
secrets_file="$(mktemp)"
trap 'rm -f "$secrets_file"' EXIT
node -e 'require("fs").writeFileSync(process.argv[1], JSON.stringify({FAUCET_PRIVATE_KEY: process.env.FAUCET_PRIVATE_KEY, SEPOLIA_RPC_URL: process.env.SEPOLIA_RPC_URL}))' "$secrets_file"
npx wrangler secret bulk "$secrets_file"
