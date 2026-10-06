#!/usr/bin/env bash
#
# Workers Builds "Deploy command" (`npm run cf:deploy`) for hot-bridge-ui, built with
# PUBLIC_NETWORK=sepolia, and hot-bridge-ui-mainnet, built with PUBLIC_NETWORK=mainnet
# as the wrangler env `mainnet`. Runs from ui/, after `npm run build`.
#
# The server routes read their secrets at runtime, and `wrangler deploy` drops bindings
# set by hand, so the secrets are kept as build variables and set again on each deploy.
set -euo pipefail

case "${PUBLIC_NETWORK:-sepolia}" in
sepolia)
	wrangler_env=()
	secrets=(SEPOLIA_RPC_URL FAUCET_PRIVATE_KEY)
	;;
mainnet)
	wrangler_env=(--env mainnet)
	secrets=(ETH_RPC_URL)
	;;
*)
	echo "PUBLIC_NETWORK must be sepolia or mainnet, as a Workers Builds build variable" >&2
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
