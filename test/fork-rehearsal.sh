#!/usr/bin/env bash
# Rehearses the mainnet deploy, then the move of the vault admin and the coupon
# signer to Safe multisigs, on anvil forks of Ethereum mainnet:
#   nix develop -c test/fork-rehearsal.sh
# FORK_URL is the mainnet RPC anvil forks from. Every transaction goes to an
# anvil this script starts on 127.0.0.1, never to FORK_URL.
set -euo pipefail
cd "$(dirname "$0")/.."

FORK_URL=${FORK_URL:-https://eth.drpc.org}
PORT=${REHEARSAL_PORT:-18545}
fork=http://127.0.0.1:$PORT
plain=http://127.0.0.1:$((PORT + 1))

unset ETH_RPC_URL SEPOLIA_RPC_URL
export FOUNDRY_DISABLE_NIGHTLY_WARNING=1

work=$(mktemp -d)
anvils=()
cleanup() {
	for pid in "${anvils[@]}"; do
		kill "$pid" 2>/dev/null || true
		wait "$pid" 2>/dev/null || true
	done
	rm -rf "$work"
}
trap cleanup EXIT
export FOUNDRY_BROADCAST=$work/broadcast FOUNDRY_CACHE_PATH=$work/cache

step() { printf '\n== %s\n' "$*"; }
fail() {
	echo "REHEARSAL FAILED: $*" >&2
	exit 1
}

start_anvil() {
	local url=$1
	shift
	anvil --host 127.0.0.1 --port "${url##*:}" --silent "$@" &
	anvils+=($!)
	for _ in $(seq 120); do
		cast chain-id --rpc-url "$url" >/dev/null 2>&1 && return
		sleep 0.5
	done
	fail "anvil on $url did not start"
}

# Anvil's own accounts, from its published test mnemonic.
keys=(
	0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
	0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d
	0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a
	0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6
	0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a
	0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba
	0x92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e
	0x4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356
	0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97
	0x2a871d0798f97d79848a013d4936a73bf4ae7b1cfd0dddbdf9ad2d28c75e78c3
)
address_of() { cast wallet address --private-key "$1"; }
deployer_key=${keys[0]} admin_key=${keys[1]} signer_key=${keys[2]}
deployer=$(address_of "$deployer_key")
admin=$(address_of "$admin_key")
signer=$(address_of "$signer_key")
test_signer=0x8E72b7568738da52ca3DCd9b24E178127A4E7d37

# The deployer signs from an encrypted keystore, as on mainnet.
cast wallet import deployer --private-key "$deployer_key" --keystore-dir "$work/keys" \
	--unsafe-password rehearsal >/dev/null
wallet=(--keystore "$work/keys/deployer" --password rehearsal)

record_value() { grep -oE "$1=0x[0-9a-fA-F]+" "$2" | tail -1 | cut -d= -f2; }
nonce() { cast nonce "$deployer" --rpc-url "$1"; }

# Runs deploy-mainnet.sh against `url`, which must refuse, naming `reason`, and
# send nothing.
expect_refusal() {
	local url=$1 reason=$2 log=$work/refusal.log before
	shift 2
	before=$(nonce "$url")
	if env ETH_RPC_URL="$url" "$@" >"$log" 2>&1; then
		cat "$log"
		fail "deploy-mainnet.sh did not refuse: $reason"
	fi
	grep -qF -- "$reason" "$log" || {
		cat "$log"
		fail "deploy-mainnet.sh refused without naming: $reason"
	}
	[[ $(nonce "$url") == "$before" ]] || fail "deploy-mainnet.sh sent a transaction while refusing: $reason"
	echo "refused, sending nothing: $reason"
}

step "Starting anvil: a fork of mainnet on $fork, and a chain 31337 on $plain"
start_anvil "$fork" --fork-url "$FORK_URL"
start_anvil "$plain"
[[ $(cast chain-id --rpc-url "$fork") == 1 ]] || fail "the fork is not chain 1"

step "The deploy refuses before it sends anything"
inputs=(ADMIN_ADDRESS="$admin" VALID_SIGNER="$signer")
expect_refusal "$plain" "the RPC answers for chain 31337, and NETWORK=mainnet is chain 1" \
	env "${inputs[@]}" ./deploy-mainnet.sh "${wallet[@]}"
expect_refusal "$fork" "VALID_SIGNER is the test signer" \
	env ADMIN_ADDRESS="$admin" VALID_SIGNER="$test_signer" ./deploy-mainnet.sh "${wallet[@]}"
expect_refusal "$fork" "ADMIN_ADDRESS is required" \
	env -u ADMIN_ADDRESS VALID_SIGNER="$signer" ./deploy-mainnet.sh "${wallet[@]}"
expect_refusal "$fork" "pass the signer as a forge wallet option" \
	env "${inputs[@]}" ./deploy-mainnet.sh
expect_refusal "$fork" "--private-key takes a raw key" \
	env "${inputs[@]}" ./deploy-mainnet.sh --private-key "$deployer_key"

step "Deploying with an EOA admin and an EOA coupon signer"
ETH_RPC_URL=$fork ADMIN_ADDRESS=$admin VALID_SIGNER=$signer ./deploy-mainnet.sh "${wallet[@]}" |
	tee "$work/deploy.log"
vault=$(record_value MAINNET_LOCK_VAULT_ADDRESS "$work/deploy.log")
order_hash=$(record_value ORDER_HASH "$work/deploy.log")
interpreter=$(record_value PUBLIC_CLAIM_INTERPRETER "$work/deploy.log")
store=$(record_value PUBLIC_CLAIM_STORE "$work/deploy.log")
expression=$(record_value PUBLIC_CLAIM_EXPRESSION "$work/deploy.log")
[[ -n $vault && -n $order_hash && -n $expression ]] || fail "the deploy printed no deploy record"

proofs() {
	REHEARSAL=true REHEARSAL_RPC_URL=$fork MAINNET_LOCK_VAULT_ADDRESS=$vault \
		ADMIN_ADDRESS=$admin REHEARSAL_DEPLOYER=$deployer REHEARSAL_SIGNER_KEY=$signer_key \
		forge test --match-path test/rehearsal/MainnetForkRehearsal.t.sol -vv "$@"
}

step "Proving the deployed bridge on the fork"
ORDER_HASH=$order_hash PUBLIC_CLAIM_INTERPRETER=$interpreter PUBLIC_CLAIM_STORE=$store \
	PUBLIC_CLAIM_EXPRESSION=$expression proofs --match-contract EoaRehearsal

safe() {
	forge script test/rehearsal/RehearsalSafe.s.sol:RehearsalSafe --rpc-url "$fork" --broadcast \
		--private-key "$deployer_key" "$@"
}
# A Safe of the accounts whose keys follow the salt.
create_safe() {
	local salt=$1 owners=() key
	shift
	for key in "$@"; do owners+=("$(address_of "$key")"); done
	safe --sig 'create(address[],uint256)' "[$(IFS=,; echo "${owners[*]}")]" "$salt" |
		grep -oE 'SAFE=0x[0-9a-fA-F]{40}' | cut -d= -f2
}
admin_owner_keys=("${keys[4]}" "${keys[5]}" "${keys[6]}")
signer_owner_keys=("${keys[7]}" "${keys[8]}" "${keys[9]}")

step "Creating two Safes on the fork, each 2 of 3 anvil accounts"
admin_safe=$(create_safe 1 "${admin_owner_keys[@]}")
signer_safe=$(create_safe 2 "${signer_owner_keys[@]}")
[[ -n $admin_safe && -n $signer_safe ]] || fail "a Safe was not created"
echo "admin Safe $admin_safe, signer Safe $signer_safe"

step "Moving the vault admin to the admin Safe, then acting through it with 2 owner signatures"
cast send --rpc-url "$fork" --private-key "$admin_key" "$vault" 'setAdmin(address)' "$admin_safe" >/dev/null
min_lock_amount=2000000000000000000
REHEARSAL_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[1]}" safe \
	--sig 'exec(address,address,bytes)' "$admin_safe" "$vault" \
	"$(cast calldata 'setMinLockAmount(uint256)' "$min_lock_amount")" >/dev/null
[[ $(cast call --rpc-url "$fork" "$vault" 'minLockAmount()(uint256)' | cut -d' ' -f1) == "$min_lock_amount" ]] ||
	fail "the admin Safe's setMinLockAmount did not take effect"
echo "minLockAmount is $min_lock_amount, set by the admin Safe"

step "Rotating the coupon signer to the signer Safe through rotate-claim-signer.sh"
rotation=(NETWORK=mainnet ETH_RPC_URL="$fork" LOCK_VAULT_ADDRESS="$vault" ORDER_HASH="$order_hash"
	CLAIM_INTERPRETER="$interpreter" CLAIM_STORE="$store" CLAIM_EXPRESSION="$expression"
	VALID_SIGNER="$signer_safe")
env "${rotation[@]}" ./rotate-claim-signer.sh | tee "$work/rotate.log"
mapfile -t calls < <(grep -oE '[0-9]+\. to 0x[0-9a-fA-F]{40}, value 0, data 0x[0-9a-fA-F]+' "$work/rotate.log")
[[ ${#calls[@]} == 2 ]] || fail "rotate-claim-signer.sh did not print the 2 Safe calls"
from_block=$(($(cast block-number --rpc-url "$fork") + 1))
for call in "${calls[@]}"; do
	to=$(grep -oE 'to 0x[0-9a-fA-F]{40}' <<<"$call" | cut -d' ' -f2)
	data=$(grep -oE 'data 0x[0-9a-fA-F]+' <<<"$call" | cut -d' ' -f2)
	REHEARSAL_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[2]}" safe \
		--sig 'exec(address,address,bytes)' "$admin_safe" "$to" "$data" >/dev/null
done
env "${rotation[@]}" ./rotate-claim-signer.sh record "$from_block" | tee "$work/rotated.log"
new_order_hash=$(record_value ORDER_HASH "$work/rotated.log")
[[ $(record_value PUBLIC_CLAIM_SIGNER "$work/rotated.log") == "$signer_safe" ]] ||
	fail "the new deploy record does not name the signer Safe"

step "Proving the Safe admin and the Safe coupon signer on the fork"
ORDER_HASH=$new_order_hash PREVIOUS_ORDER_HASH=$order_hash \
	PUBLIC_CLAIM_INTERPRETER=$(record_value PUBLIC_CLAIM_INTERPRETER "$work/rotated.log") \
	PUBLIC_CLAIM_STORE=$(record_value PUBLIC_CLAIM_STORE "$work/rotated.log") \
	PUBLIC_CLAIM_EXPRESSION=$(record_value PUBLIC_CLAIM_EXPRESSION "$work/rotated.log") \
	ADMIN_SAFE=$admin_safe SIGNER_SAFE=$signer_safe REHEARSAL_MIN_LOCK_AMOUNT=$min_lock_amount \
	REHEARSAL_SIGNER_OWNER_KEYS="${signer_owner_keys[1]},${signer_owner_keys[2]}" \
	REHEARSAL_SIGNER_ONE_OWNER_KEY="${signer_owner_keys[0]}" \
	REHEARSAL_ADMIN_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[1]}" \
	proofs --match-contract SafeRehearsal

step "Fork rehearsal passed"
