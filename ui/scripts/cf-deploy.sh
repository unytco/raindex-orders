#!/usr/bin/env bash
#
# Cloudflare Workers Builds "Deploy command" for the bridge website's two Workers:
# hot-bridge-ui, built with PUBLIC_NETWORK=sepolia, and hot-bridge-ui-mainnet, built
# with PUBLIC_NETWORK=mainnet and deployed as the wrangler env `mainnet`. Point each
# project's Deploy command at this (`npm run cf:deploy`); the Build command stays
# `npm run build`. Runs from the same dir as the build (ui/).
#
# Why: the server routes read their RPC URL and faucet key at runtime via
# $env/dynamic/private, i.e. from the Worker's runtime secret bindings, not the build.
# `wrangler deploy` resets dashboard-set bindings on every push, so runtime values added
# by hand get wiped. Workers Builds *build variables*, by contrast, persist. So we keep
# the secrets as build variables and re-apply them as runtime secrets here on every
# deploy: the wipe heals itself and nothing needs managing in the CF dashboard beyond
# pointing the Deploy command at this script once.
set -euo pipefail

case "${PUBLIC_NETWORK:-}" in
sepolia)
	wrangler_env=()
	secrets=(SEPOLIA_RPC_URL FAUCET_PRIVATE_KEY)
	;;
mainnet)
	wrangler_env=(--env mainnet)
	secrets=(ETH_RPC_URL)
	;;
*)
	echo "set PUBLIC_NETWORK to sepolia or mainnet as a Workers Builds build variable" >&2
	exit 1
	;;
esac
for secret in "${secrets[@]}"; do
	if [[ -z ${!secret:-} ]]; then
		echo "set $secret as a Workers Builds build variable" >&2
		exit 1
	fi
done

# Ship the built worker. --keep-vars stops the deploy from dropping bindings not
# declared in wrangler.jsonc, so any secrets already present survive with no gap.
npx wrangler deploy --keep-vars ${wrangler_env[@]+"${wrangler_env[@]}"}

# Re-apply the runtime secrets from the build vars. Built with node for safe JSON
# escaping, written to a 0600 temp file, removed on exit, never echoed.
secrets_file="$(mktemp)"
trap 'rm -f "$secrets_file"' EXIT
node -e '
	const [file, ...names] = process.argv.slice(1)
	require("fs").writeFileSync(file, JSON.stringify(Object.fromEntries(names.map(n => [n, process.env[n]]))))
' "$secrets_file" "${secrets[@]}"
npx wrangler secret bulk "$secrets_file" ${wrangler_env[@]+"${wrangler_env[@]}"}
