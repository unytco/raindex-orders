#!/usr/bin/env bash
# Rehearses the mainnet launch on an anvil fork of Ethereum mainnet: one Safe,
# created by `deploy-mainnet.sh safe`, is the vault admin from the deploy's first
# transaction and the coupon signer, and the orchestrator collects its owners'
# signatures from signer services (mock ones, which check no Holochain). Then the
# separate shape: an admin Safe, a key as the coupon signer, and its move to a
# signer Safe of its own:
#   nix develop -c test/fork-rehearsal.sh
# FORK_URL is the mainnet RPC anvil forks. Every transaction goes to an anvil
# this script starts on 127.0.0.1, never to FORK_URL.
set -euo pipefail
cd "$(dirname "$0")/.."

FORK_URL=${FORK_URL:-https://ethereum-rpc.publicnode.com}
ORCHESTRATOR=${ORCHESTRATOR:-bridge-orchestrator/target/debug/bridge-orchestrator}
PORT=${REHEARSAL_PORT:-18545}
fork=http://127.0.0.1:$PORT
plain=http://127.0.0.1:$((PORT + 1))

unset ETH_RPC_URL SEPOLIA_RPC_URL ADMIN_ADDRESS VALID_SIGNER ADMIN_SAFE_THRESHOLD ADMIN_SAFE_OWNERS \
	UNSAFE_ALLOW_SINGLE_KEY_ADMIN UNSAFE_ALLOW_ONE_PARTY_SIGNER SAFE_OWNERS SAFE_THRESHOLD SAFE_SALT_NONCE
export FOUNDRY_DISABLE_NIGHTLY_WARNING=1

work=$(mktemp -d)
pids=()
cleanup() {
	for pid in "${pids[@]}"; do
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
	local url=$1 pid
	shift
	cast chain-id --rpc-url "$url" >/dev/null 2>&1 && fail "something already answers on $url"
	anvil --host 127.0.0.1 --port "${url##*:}" --silent "$@" &
	pid=$!
	pids+=("$pid")
	for _ in $(seq 120); do
		kill -0 "$pid" 2>/dev/null || fail "anvil on $url exited"
		[[ $(cast rpc web3_clientVersion --rpc-url "$url" 2>/dev/null) == '"anvil/'* ]] && return
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
# Anvil's accounts carry EIP-7702 delegations on mainnet, which Raindex V6 refuses
# as coupon signers (it asks a delegated key's delegate through EIP-1271), so the
# coupon signers are fresh keys. A Safe's owners sign with ecrecover, so anvil's
# accounts serve as owners.
fresh_key() { cast wallet new --json | jq -r '.data[0].private_key'; }
deployer_key=${keys[0]} admin_key=${keys[1]} signer_key=$(fresh_key) other_signer_key=$(fresh_key)
deployer=$(address_of "$deployer_key")
admin=$(address_of "$admin_key")
signer=$(address_of "$signer_key")
test_signer=0x8E72b7568738da52ca3DCd9b24E178127A4E7d37

# The deployer signs from an encrypted keystore, as on mainnet.
cast wallet import deployer --private-key "$deployer_key" --keystore-dir "$work/keys" \
	--unsafe-password rehearsal >/dev/null
wallet=(--keystore "$work/keys/deployer" --password rehearsal)

record_value() { grep -oE "$1=0x[0-9a-fA-F]+" "$2" | tail -1 | cut -d= -f2 || true; }
nonce() { cast nonce "$deployer" --rpc-url "$1"; }
next_block() { echo $(($(cast block-number --rpc-url "$fork") + 1)); }

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

[[ -x $ORCHESTRATOR ]] ||
	fail "build the orchestrator first: cargo build in bridge-orchestrator, on its pinned toolchain, or set ORCHESTRATOR"
ORCHESTRATOR=$(realpath "$ORCHESTRATOR")

step "Starting anvil: a fork of mainnet on $fork, and a chain 31337 on $plain"
start_anvil "$fork" --fork-url "$FORK_URL"
start_anvil "$plain"
[[ $(cast chain-id --rpc-url "$fork") == 1 ]] || fail "the fork is not chain 1"

safe() {
	forge script test/rehearsal/RehearsalSafe.s.sol:RehearsalSafe --rpc-url "$fork" --broadcast \
		--private-key "$deployer_key" "$@"
}
create_safe() {
	local threshold=$1 salt=$2 owners=() key
	shift 2
	for key in "$@"; do owners+=("$(address_of "$key")"); done
	safe --sig 'create(address[],uint256,uint256)' "[$(IFS=,; echo "${owners[*]}")]" "$threshold" "$salt" |
		grep -oE 'SAFE=0x[0-9a-fA-F]{40}' | cut -d= -f2 || true
}
owners_of() {
	local owners=() key
	for key in "$@"; do owners+=("$(address_of "$key")"); done
	IFS=,
	echo "${owners[*]}"
}
admin_owner_keys=("${keys[4]}" "${keys[5]}" "${keys[6]}")
signer_owner_keys=("${keys[7]}" "${keys[8]}" "${keys[9]}")

step "Creating the bridge's Safe with deploy-mainnet.sh safe: 2 of 3 owners, through Safe's factory"
bridge_owner_keys=("$(fresh_key)" "$(fresh_key)" "$(fresh_key)")
bridge_owners=$(owners_of "${bridge_owner_keys[@]}")
create_bridge_safe() {
	env ETH_RPC_URL="$fork" SAFE_OWNERS="$bridge_owners" "$@" ./deploy-mainnet.sh safe "${wallet[@]}"
}
before=$(nonce "$fork")
create_bridge_safe SAFE_THRESHOLD=1 >"$work/refusal.log" 2>&1 && fail "deploy-mainnet.sh safe took a threshold of 1"
grep -qF "SAFE_THRESHOLD is how many owners must sign, 2 to the 3 SAFE_OWNERS lists" "$work/refusal.log" || {
	cat "$work/refusal.log"
	fail "deploy-mainnet.sh safe refused a threshold of 1 without naming it"
}
[[ $(nonce "$fork") == "$before" ]] || fail "deploy-mainnet.sh safe sent a transaction while refusing"
echo "refused, sending nothing: a Safe of threshold 1"
create_bridge_safe SAFE_THRESHOLD=2 | tee "$work/safe.log"
bridge_safe=$(record_value SAFE "$work/safe.log")
[[ -n $bridge_safe && $(cast code "$bridge_safe" --rpc-url "$fork") != 0x ]] || fail "the bridge's Safe was not created"
[[ $(nonce "$fork") == $((before + 1)) ]] || fail "deploy-mainnet.sh safe sent other than one transaction"
[[ $(cast call --rpc-url "$fork" "$bridge_safe" 'getThreshold()(uint256)') == 2 ]] || fail "the bridge's Safe's threshold is not 2"
[[ $(cast storage "$bridge_safe" 0 --rpc-url "$fork") == "$(cast to-uint256 0x41675C099F32341bf84BFc5382aF534df5C7461a)" ]] ||
	fail "the bridge's Safe does not run the Safe 1.4.1 singleton"
before=$(nonce "$fork")
create_bridge_safe SAFE_THRESHOLD=2 >"$work/again.log"
grep -qF "The Safe exists, with these owners and threshold. Nothing was sent: $bridge_safe" "$work/again.log" ||
	fail "deploy-mainnet.sh safe did not find the Safe it created"
[[ $(nonce "$fork") == "$before" ]] || fail "deploy-mainnet.sh safe sent a transaction for a Safe that exists"
echo "found again, sending nothing: $bridge_safe"

step "Creating Safes of 3 anvil accounts on the fork: two that need 2 signatures, one that needs 1, one with no fallback handler"
admin_safe=$(create_safe 2 1 "${admin_owner_keys[@]}")
signer_safe=$(create_safe 2 2 "${signer_owner_keys[@]}")
single_safe=$(create_safe 1 3 "${signer_owner_keys[@]}")
bare_safe=$(safe --sig 'createWithoutHandler(address[],uint256,uint256)' "[$(owners_of "${signer_owner_keys[@]}")]" 2 4 |
	grep -oE 'SAFE=0x[0-9a-fA-F]{40}' | cut -d= -f2 || true)
[[ -n $admin_safe && -n $signer_safe && -n $single_safe && -n $bare_safe ]] || fail "a Safe was not created"
admin_owners=$(owners_of "${admin_owner_keys[@]}")
signer_owners=$(owners_of "${signer_owner_keys[@]}")
echo "admin Safe $admin_safe, signer Safe $signer_safe"

step "The deploy refuses before it sends anything"
inputs=(ADMIN_ADDRESS="$bridge_safe" ADMIN_SAFE_THRESHOLD=2 ADMIN_SAFE_OWNERS="$bridge_owners" VALID_SIGNER="$bridge_safe")
deploy=(./deploy-mainnet.sh "${wallet[@]}")
expect_refusal "$plain" "the RPC answers for chain 31337, and NETWORK=mainnet is chain 1" \
	env "${inputs[@]}" "${deploy[@]}"
expect_refusal "$fork" "VALID_SIGNER is the test signer" \
	env "${inputs[@]}" VALID_SIGNER="$test_signer" "${deploy[@]}"
expect_refusal "$fork" "ADMIN_ADDRESS is required" \
	env -u ADMIN_ADDRESS VALID_SIGNER="$bridge_safe" "${deploy[@]}"
expect_refusal "$fork" "VALID_SIGNER is required" \
	env -u VALID_SIGNER ADMIN_ADDRESS="$bridge_safe" "${deploy[@]}"
expect_refusal "$fork" "pass the signer as a forge wallet option" \
	env "${inputs[@]}" ./deploy-mainnet.sh
expect_refusal "$fork" "--private-key takes a raw key" \
	env "${inputs[@]}" ./deploy-mainnet.sh --private-key "$deployer_key"
# The refusal must not rest on keys[2]'s live mainnet code, which its delegation's
# owner can change or clear: give it a delegation designator on the fork.
delegated=$(address_of "${keys[2]}")
cast rpc anvil_setCode "$delegated" "0xef0100${deployer#0x}" --rpc-url "$fork" >/dev/null
expect_refusal "$fork" "VALID_SIGNER has an EIP-7702 delegation" \
	env "${inputs[@]}" VALID_SIGNER="$delegated" "${deploy[@]}"
expect_refusal "$fork" "VALID_SIGNER is a contract but not a Safe whose threshold is 2 to 20" \
	env "${inputs[@]}" VALID_SIGNER="$single_safe" "${deploy[@]}"
expect_refusal "$fork" "VALID_SIGNER is a Safe whose fallback handler does not hash coupons" \
	env "${inputs[@]}" VALID_SIGNER="$bare_safe" "${deploy[@]}"
echo "-- the vault admin must be a Safe of threshold 2 or more, the one meant"
expect_refusal "$fork" "ADMIN_ADDRESS $admin is a key, not a Safe" \
	env "${inputs[@]}" ADMIN_ADDRESS="$admin" "${deploy[@]}"
expect_refusal "$fork" "ADMIN_ADDRESS $deployer is a key, not a Safe" \
	env "${inputs[@]}" ADMIN_ADDRESS="$deployer" "${deploy[@]}"
expect_refusal "$fork" "ADMIN_ADDRESS is the deployer" \
	env "${inputs[@]}" ADMIN_ADDRESS="$deployer" UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes "${deploy[@]}"
expect_refusal "$fork" "is a Safe of threshold 1 with 3 owners, which one key controls" \
	env "${inputs[@]}" ADMIN_ADDRESS="$single_safe" ADMIN_SAFE_THRESHOLD=1 ADMIN_SAFE_OWNERS="$signer_owners" "${deploy[@]}"
expect_refusal "$fork" "is a contract but not a Safe" \
	env "${inputs[@]}" ADMIN_ADDRESS=0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20 "${deploy[@]}"
expect_refusal "$fork" "ADMIN_SAFE_THRESHOLD is required" \
	env ADMIN_ADDRESS="$bridge_safe" ADMIN_SAFE_OWNERS="$bridge_owners" VALID_SIGNER="$bridge_safe" "${deploy[@]}"
expect_refusal "$fork" "is a Safe of threshold 2 with 3 owners, not ADMIN_SAFE_THRESHOLD=3" \
	env "${inputs[@]}" ADMIN_SAFE_THRESHOLD=3 "${deploy[@]}"
expect_refusal "$fork" "ADMIN_SAFE_OWNERS is required" \
	env ADMIN_ADDRESS="$bridge_safe" ADMIN_SAFE_THRESHOLD=2 VALID_SIGNER="$bridge_safe" "${deploy[@]}"
expect_refusal "$fork" "ADMIN_SAFE_OWNERS does not list the owners of ADMIN_ADDRESS $bridge_safe" \
	env "${inputs[@]}" ADMIN_SAFE_OWNERS="$signer_owners" "${deploy[@]}"
expect_refusal "$fork" "ADMIN_SAFE_OWNERS does not list the owners of ADMIN_ADDRESS $bridge_safe" \
	env "${inputs[@]}" ADMIN_SAFE_OWNERS="$(owners_of "${bridge_owner_keys[0]}" "${bridge_owner_keys[1]}")" "${deploy[@]}"

# Reads the deploy record `log` into the variables the proofs and the
# orchestrator take.
read_record() {
	local log=$1
	vault=$(record_value MAINNET_LOCK_VAULT_ADDRESS "$log")
	order_hash=$(record_value ORDER_HASH "$log")
	interpreter=$(record_value PUBLIC_CLAIM_INTERPRETER "$log")
	store=$(record_value PUBLIC_CLAIM_STORE "$log")
	bytecode=$(record_value PUBLIC_CLAIM_BYTECODE "$log")
	input_token=$(record_value PUBLIC_CLAIM_INPUT_TOKEN "$log")
	bridge_deployer=$(record_value HoloBridgeDeployer "$log")
	[[ -n $vault && -n $order_hash && -n $interpreter && -n $store && -n $bytecode && -n $input_token &&
		-n $bridge_deployer ]] || fail "the deploy printed no complete deploy record"
}


proofs() {
	REHEARSAL=true REHEARSAL_RPC_URL=$fork MAINNET_LOCK_VAULT_ADDRESS=$vault \
		REHEARSAL_DEPLOYER=$deployer REHEARSAL_SIGNER_KEY=$signer_key \
		forge test --match-path test/rehearsal/MainnetForkRehearsal.t.sol -vv "$@"
}

# Starts `bridge-orchestrator run` on the fork with the deploy record in `record`
# and the key settings `settings`, with no Holochain node: it gets past its
# startup checks only to stop where it needs the node. The conductor config it
# is given is durable and names a lair socket that is not there, which is where
# it stops. Prints what it logged.
orchestrate() {
	local record=$1 dir
	shift
	dir=$(mktemp -d -p "$work")
	printf 'keystore:\n  type: lair_server\n  connection_url: unix:///%s/no-lair?k=abc\ndb_sync_level: Full\n' \
		"$dir" >"$dir/conductor-config.yaml"
	(
		cd "$dir"
		mapfile -t lines < <(sed -n '/bridge-orchestrator:/,/website:/p' "$record" | grep -oE '[A-Z_]+=[^ ]+')
		# Mainnet refuses to start without screening and a denylist.
		env -i PATH="$PATH" "${lines[@]}" ETH_RPC_URL="$fork" \
			SCREENING=on SCREENING_DENYLIST=0x000000000000000000000000000000000000dEaD "$@" \
			HOLOCHAIN_BRIDGING_AGENT_PUBKEY=uhCAkYoBhEs3GyOWslej78VfMRmSSdc2TXsRQmqFn5b3v8jl58Kkj \
			DB_PATH="$dir/bridge.db" CONDUCTOR_CONFIG="$dir/conductor-config.yaml" \
			LAIR_PASSPHRASE_FILE="$dir/no-passphrase" "$ORCHESTRATOR" run 2>&1 || true
	)
}
# The orchestrator must pass its startup checks with `record` and the key
# settings that follow.
expect_startup() {
	local record=$1 what=$2
	shift 2
	orchestrate "$record" "$@" >"$work/orchestrator.log"
	grep -qF "startup checks passed" "$work/orchestrator.log" || {
		cat "$work/orchestrator.log"
		fail "the orchestrator did not pass its startup checks $what"
	}
	echo "passed its startup checks $what"
}
# The orchestrator must refuse to start, naming `reason`.
expect_orchestrator_refusal() {
	local record=$1 reason=$2
	shift 2
	orchestrate "$record" "$@" >"$work/orchestrator.log"
	grep -qF "startup checks passed" "$work/orchestrator.log" && {
		cat "$work/orchestrator.log"
		fail "the orchestrator started: $reason"
	}
	grep -qF -- "$reason" "$work/orchestrator.log" || {
		cat "$work/orchestrator.log"
		fail "the orchestrator refused without naming: $reason"
	}
	echo "refused: $reason"
}


step "Deploying the bridge: the bridge's Safe is the vault admin from the first transaction and the coupon signer"
deploy_block=$(next_block)
ETH_RPC_URL=$fork env "${inputs[@]}" ./deploy-mainnet.sh "${wallet[@]}" | tee "$work/one.log"
grep -qF "ADMIN_ADDRESS $bridge_safe is a Safe of threshold 2 with 3 owners" "$work/one.log" ||
	fail "the deploy did not print the Safe's threshold and owners"
grep -qF "VALID_SIGNER is ADMIN_ADDRESS $bridge_safe: one Safe is the vault admin and the coupon signer" "$work/one.log" ||
	fail "the deploy did not say one Safe holds both roles"
[[ $(grep -c "succeeded in block" "$work/one.log") == 1 ]] || fail "the deploy sent more than one transaction"
read_record "$work/one.log"
[[ $(cast call --rpc-url "$fork" "$vault" 'admin()(address)') == "$bridge_safe" ]] ||
	fail "the vault's admin is not the bridge's Safe"
[[ $(record_value PUBLIC_CLAIM_SIGNER "$work/one.log") == "$bridge_safe" ]] ||
	fail "the deploy record does not name the bridge's Safe as the coupon signer"

step "Starting mock signer services for the Safe's three owners (no Holochain checks)"
service_port=$((PORT + 10))
# Starts a mock signer service for owner key `key` on `port`, answering as `mode`.
start_service() {
	local port=$1 key=$2 mode=${3:-sign} log=$work/service-$1.log
	env -i PATH="$PATH" OWNER_KEY="$key" SAFE="$bridge_safe" CHAIN_ID=1 PORT="$port" MODE="$mode" \
		ORDER_HASH="$order_hash" ORDER_OWNER="$vault" ORDERBOOK_ADDRESS="$(record_value ORDERBOOK_ADDRESS "$work/one.log")" \
		TOKEN_ADDRESS="$(record_value TOKEN_ADDRESS "$work/one.log")" VAULT_ID="$(record_value VAULT_ID "$work/one.log")" \
		node ui/scripts/mock-signer-service.mjs >"$log" 2>&1 &
	pids+=("$!")
	for _ in $(seq 40); do
		curl -sf "http://127.0.0.1:$port/signer" >/dev/null && return
		sleep 0.25
	done
	cat "$log"
	fail "the mock signer service on $port did not start"
}
for i in 0 1 2; do start_service $((service_port + i)) "${bridge_owner_keys[$i]}"; done
start_service $((service_port + 3)) "${bridge_owner_keys[0]}" wrong-key
start_service $((service_port + 4)) "${bridge_owner_keys[2]}" refuse
down_port=$((service_port + 9))
bridge_owner() { address_of "${bridge_owner_keys[$1]}"; }
at() { echo "$(bridge_owner "$1")=http://127.0.0.1:$2"; }

step "Starting the orchestrator for the bridge's Safe"
# Owner keys held by the orchestrator are one party, and here they are the vault
# admin's keys too: mainnet refuses them unless UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes.
expect_orchestrator_refusal "$work/one.log" \
	"this orchestrator holds the keys of 2 owners of CLAIM_SIGNER $bridge_safe, a Safe of threshold 2, and signs as all of them, which is one party, not 2; CLAIM_SIGNER $bridge_safe is also the vault's admin" \
	SAFE_OWNER_KEYS="${bridge_owner_keys[0]},${bridge_owner_keys[1]}"
expect_startup "$work/one.log" "with the keys of 2 of the Safe's 3 owners and UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes" \
	SAFE_OWNER_KEYS="${bridge_owner_keys[0]},${bridge_owner_keys[1]}" UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes
grep -qF "which is one party, not 2" "$work/orchestrator.log" || fail "the orchestrator did not say it is one party"
expect_startup "$work/one.log" "with the Safe's 3 owners at their signer services" \
	SAFE_OWNER_SERVICES="$(at 0 $service_port),$(at 1 $((service_port + 1))),$(at 2 $((service_port + 2)))"
grep -qF "the claim order took the probe signed by 2 owners of CLAIM_SIGNER $bridge_safe, and refused it signed by 1" \
	"$work/orchestrator.log" || fail "the orchestrator did not probe with 2 owner signatures and with 1"
grep -q "POST /sign -> 200" "$work/service-$service_port.log" || fail "the first signer service did not sign the probe"
expect_startup "$work/one.log" "with one owner's signer service down" \
	SAFE_OWNER_SERVICES="$(at 0 $down_port),$(at 1 $((service_port + 1))),$(at 2 $((service_port + 2)))"
grep -qF "the signer service for $(bridge_owner 0) at 127.0.0.1:$down_port could not be reached" "$work/orchestrator.log" ||
	fail "the orchestrator did not log the signer service that is down"
expect_startup "$work/one.log" "with one owner's signer service refusing" \
	SAFE_OWNER_SERVICES="$(at 0 $service_port),$(at 1 $((service_port + 1))),$(at 2 $((service_port + 4)))"
grep -qF "refused: refused_by_operator" "$work/orchestrator.log" || fail "the orchestrator did not log the refusal's code"
expect_orchestrator_refusal "$work/one.log" \
	"CLAIM_SIGNER $bridge_safe is also the vault's admin, so the 1 owner keys in SAFE_OWNER_KEYS also approve its admin transactions" \
	SAFE_OWNER_KEYS="${bridge_owner_keys[2]}" SAFE_OWNER_SERVICES="$(at 1 $((service_port + 1)))"
expect_startup "$work/one.log" "with one owner key held here, one owner's signer service and UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes" \
	SAFE_OWNER_KEYS="${bridge_owner_keys[2]}" SAFE_OWNER_SERVICES="$(at 1 $((service_port + 1)))" \
	UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes
expect_orchestrator_refusal "$work/one.log" \
	"1 of the owners this orchestrator asks can sign now, and CLAIM_SIGNER $bridge_safe is a Safe of threshold 2" \
	SAFE_OWNER_SERVICES="$(at 0 $down_port),$(at 1 $((service_port + 1)))"
expect_orchestrator_refusal "$work/one.log" \
	"answered a signature that is not $(bridge_owner 0)'s over the coupon's SafeMessage hash" \
	SAFE_OWNER_SERVICES="$(at 0 $((service_port + 3))),$(at 1 $((service_port + 1)))"
expect_orchestrator_refusal "$work/one.log" \
	"signs as $(bridge_owner 2) for $bridge_safe on chain 1, and SAFE_OWNER_SERVICES names it for $(bridge_owner 0)" \
	SAFE_OWNER_SERVICES="$(at 0 $((service_port + 2))),$(at 1 $((service_port + 1)))"
expect_orchestrator_refusal "$work/one.log" \
	"CLAIM_SIGNER $bridge_safe is a contract, such as a Safe, and SIGNER_PRIVATE_KEY signs as a key" \
	SIGNER_PRIVATE_KEY="${bridge_owner_keys[0]}"
expect_orchestrator_refusal "$work/one.log" \
	"SAFE_OWNER_KEYS holds the key of $(address_of "${admin_owner_keys[0]}"), which is not an owner of CLAIM_SIGNER $bridge_safe" \
	SAFE_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[1]}"

step "Proving the launched bridge on the fork: one Safe is the vault admin and the coupon signer"
ORDER_HASH=$order_hash PUBLIC_CLAIM_INTERPRETER=$interpreter PUBLIC_CLAIM_STORE=$store \
	PUBLIC_CLAIM_BYTECODE=$bytecode PUBLIC_CLAIM_INPUT_TOKEN=$input_token \
	BRIDGE_SAFE=$bridge_safe HOLO_BRIDGE_DEPLOYER=$bridge_deployer REHEARSAL_DEPLOY_BLOCK=$deploy_block \
	REHEARSAL_BRIDGE_OWNER_KEYS="${bridge_owner_keys[2]},${bridge_owner_keys[0]}" \
	REHEARSAL_BRIDGE_ONE_OWNER_KEY="${bridge_owner_keys[1]}" \
	proofs --match-contract OneSafeRehearsal

step "Deploying a second bridge in the separate shape: the admin Safe, and a key as the coupon signer"
inputs=(ADMIN_ADDRESS="$admin_safe" ADMIN_SAFE_THRESHOLD=2 ADMIN_SAFE_OWNERS="$admin_owners" VALID_SIGNER="$signer")
deploy_block=$(next_block)
ETH_RPC_URL=$fork env "${inputs[@]}" ./deploy-mainnet.sh "${wallet[@]}" | tee "$work/deploy.log"
grep -qF "ADMIN_ADDRESS $admin_safe is a Safe of threshold 2 with 3 owners" "$work/deploy.log" ||
	fail "the deploy did not print the admin Safe's threshold and owners"
[[ $(grep -c "succeeded in block" "$work/deploy.log") == 1 ]] || fail "the deploy sent more than one transaction"
read_record "$work/deploy.log"
[[ $(cast call --rpc-url "$fork" "$vault" 'admin()(address)') == "$admin_safe" ]] ||
	fail "the vault's admin is not the admin Safe"

step "Starting the orchestrator against the deployed claim order"
expect_startup "$work/deploy.log" "with the record and the coupon signer's key" SIGNER_PRIVATE_KEY="$signer_key"
expect_orchestrator_refusal \
	<(sed "s/CLAIM_SIGNER=0x[0-9a-fA-F]*/CLAIM_SIGNER=$(address_of "$other_signer_key")/" "$work/deploy.log") \
	"does not accept coupons from CLAIM_SIGNER $(address_of "$other_signer_key"): it answered \"Wrong signer\"" \
	SIGNER_PRIVATE_KEY="$other_signer_key"
expect_orchestrator_refusal "$work/deploy.log" \
	"CLAIM_SIGNER $signer has no contract, so it is a key, not a Safe, and SAFE_OWNER_KEYS and SAFE_OWNER_SERVICES sign for a Safe" \
	SAFE_OWNER_KEYS="${signer_owner_keys[0]},${signer_owner_keys[1]}"

step "Proving the launched bridge on the fork: the Safe admin and the key coupon signer"
ORDER_HASH=$order_hash PUBLIC_CLAIM_INTERPRETER=$interpreter PUBLIC_CLAIM_STORE=$store \
	PUBLIC_CLAIM_BYTECODE=$bytecode PUBLIC_CLAIM_INPUT_TOKEN=$input_token \
	ADMIN_SAFE=$admin_safe HOLO_BRIDGE_DEPLOYER=$bridge_deployer REHEARSAL_DEPLOY_BLOCK=$deploy_block \
	REHEARSAL_ADMIN_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[1]}" \
	proofs --match-contract LaunchRehearsal

step "Acting as the admin Safe on the fork with 2 owner signatures"
min_lock_amount=2000000000000000000
REHEARSAL_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[1]}" safe \
	--sig 'exec(address,address,bytes)' "$admin_safe" "$vault" \
	"$(cast calldata 'setMinLockAmount(uint256)' "$min_lock_amount")" >/dev/null
[[ $(cast call --rpc-url "$fork" "$vault" 'minLockAmount()(uint256)' | cut -d' ' -f1) == "$min_lock_amount" ]] ||
	fail "the admin Safe's setMinLockAmount did not take effect"
echo "minLockAmount is $min_lock_amount, set by the admin Safe"

step "Rotating the coupon signer to the signer Safe through rotate-claim-signer.sh"
rotation=(NETWORK=mainnet ETH_RPC_URL="$fork" MAINNET_LOCK_VAULT_ADDRESS="$vault" ORDER_HASH="$order_hash"
	PUBLIC_CLAIM_BYTECODE="$bytecode" PUBLIC_CLAIM_INPUT_TOKEN="$input_token" VALID_SIGNER="$signer_safe")
if env "${rotation[@]}" VALID_SIGNER="$test_signer" ./rotate-claim-signer.sh >"$work/refusal.log" 2>&1; then
	fail "rotate-claim-signer.sh took the test signer on mainnet"
fi
grep -qF "VALID_SIGNER is the test signer" "$work/refusal.log" || fail "rotate-claim-signer.sh refused without naming the test signer"
echo "refused: the test signer as the new coupon signer"
if env "${rotation[@]}" VALID_SIGNER="$single_safe" ./rotate-claim-signer.sh >"$work/refusal.log" 2>&1; then
	fail "rotate-claim-signer.sh took a Safe of threshold 1"
fi
grep -qF "not a Safe whose threshold is 2 to 20" "$work/refusal.log" || fail "rotate-claim-signer.sh refused a threshold 1 Safe without naming it"
echo "refused: a Safe of threshold 1 as the new coupon signer"
safe_calls() { grep -oE '[0-9]+\. to 0x[0-9a-fA-F]{40}, value 0, data 0x[0-9a-fA-F]+' "$1" || true; }
# Executes each printed call from the admin Safe, signed by two of its owners.
execute() {
	local call to data
	for call in "$@"; do
		to=$(grep -oE 'to 0x[0-9a-fA-F]{40}' <<<"$call" | cut -d' ' -f2)
		data=$(grep -oE 'data 0x[0-9a-fA-F]+' <<<"$call" | cut -d' ' -f2)
		REHEARSAL_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[2]}" safe \
			--sig 'exec(address,address,bytes)' "$admin_safe" "$to" "$data" >/dev/null
	done
}
env "${rotation[@]}" ./rotate-claim-signer.sh | tee "$work/rotate.log"
mapfile -t calls < <(safe_calls "$work/rotate.log")
[[ ${#calls[@]} == 2 ]] || fail "rotate-claim-signer.sh did not print the 2 Safe calls"

# Runs the printed calls on a snapshot of the fork, expects record to refuse
# naming `reason`, then reverts the fork.
expect_record_refusal() {
	local reason=$1 snapshot from_block
	shift
	snapshot=$(cast rpc evm_snapshot --rpc-url "$fork" | tr -d '"')
	from_block=$(next_block)
	execute "$@"
	if env "${rotation[@]}" ./rotate-claim-signer.sh record "$from_block" >"$work/refusal.log" 2>&1; then
		fail "the record passed: $reason"
	fi
	grep -qF "$reason" "$work/refusal.log" || fail "the record refused without naming: $reason"
	[[ $(cast rpc evm_revert "$snapshot" --rpc-url "$fork") == true ]] || fail "the fork did not revert"
	echo "refused: a record when $reason"
}

# A V6 order is its bytecode, so the same signer gives the same order, which the
# orderbook already holds: the second order is for another signer.
env "${rotation[@]}" VALID_SIGNER="$(address_of "$other_signer_key")" ./rotate-claim-signer.sh >"$work/rogue.log" ||
	fail "rotate-claim-signer.sh did not print the calls for a second order"
mapfile -t rogue < <(safe_calls "$work/rogue.log")
[[ ${#rogue[@]} == 2 ]] || fail "rotate-claim-signer.sh did not print the 2 calls for a second order"
expect_record_refusal "the vault holds another order" "${rogue[0]}" "${calls[@]}"

# The claim order's call names the network's interpreter; one naming another
# address adds, since the orderbook does not check it, and the record must refuse it.
padded() { printf '%064s' "$(tr 'A-F' 'a-f' <<<"${1#0x}")" | tr ' ' 0; }
other_interpreter=$(address_of "${keys[3]}")
misrun=${calls[0]//$(padded "$interpreter")/$(padded "$other_interpreter")}
[[ $misrun != "${calls[0]}" ]] || fail "the claim order's call names no interpreter to replace"
expect_record_refusal "not run by the network's interpreter and store" "$misrun" "${calls[1]}"

from_block=$(next_block)
execute "${calls[@]}"
if env "${rotation[@]}" VALID_SIGNER="$signer" ./rotate-claim-signer.sh record "$from_block" >"$work/refusal.log" 2>&1; then
	fail "the record named a signer the new claim order does not accept"
fi
grep -qF "is not the claim expression for VALID_SIGNER" "$work/refusal.log" ||
	fail "the record refused another signer without naming the expression"
echo "refused: a record naming a signer the claim order does not accept"
env "${rotation[@]}" ./rotate-claim-signer.sh record "$from_block" | tee "$work/rotated.log"
new_order_hash=$(record_value ORDER_HASH "$work/rotated.log")
new_bytecode=$(record_value PUBLIC_CLAIM_BYTECODE "$work/rotated.log")
[[ -n $new_order_hash && -n $new_bytecode ]] || fail "the rotation printed no complete deploy record"
[[ $(record_value PUBLIC_CLAIM_INPUT_TOKEN "$work/rotated.log") == "$input_token" ]] ||
	fail "the new deploy record does not keep the claim input token"
[[ $(record_value PUBLIC_CLAIM_SIGNER "$work/rotated.log") == "$signer_safe" ]] ||
	fail "the new deploy record does not name the signer Safe"

step "Starting the orchestrator for the Safe coupon signer with its owners' keys"
two_owner_keys="${signer_owner_keys[1]},${signer_owner_keys[2]}"
expect_orchestrator_refusal "$work/rotated.log" \
	"this orchestrator holds the keys of 2 owners of CLAIM_SIGNER $signer_safe, a Safe of threshold 2, and signs as all of them, which is one party, not 2. Mainnet takes owners at independent signer services" \
	SAFE_OWNER_KEYS="$two_owner_keys"
expect_startup "$work/rotated.log" "with the keys of 2 of the signer Safe's 3 owners and UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes" \
	SAFE_OWNER_KEYS="$two_owner_keys" UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes
grep -qF "CLAIM_SIGNER is a Safe of threshold 2" "$work/orchestrator.log" ||
	fail "the orchestrator did not log the Safe's threshold"
expect_orchestrator_refusal "$work/rotated.log" \
	"CLAIM_SIGNER $signer_safe is a contract, such as a Safe, and SIGNER_PRIVATE_KEY signs as a key" \
	SIGNER_PRIVATE_KEY="$signer_key"
expect_orchestrator_refusal "$work/rotated.log" \
	"SAFE_OWNER_KEYS holds 1 keys" \
	SAFE_OWNER_KEYS="${signer_owner_keys[0]}"
expect_orchestrator_refusal "$work/rotated.log" \
	"SAFE_OWNER_KEYS holds 3 keys, and CLAIM_SIGNER $signer_safe is a Safe of threshold 2" \
	SAFE_OWNER_KEYS="$(IFS=,; echo "${signer_owner_keys[*]}")"
expect_orchestrator_refusal "$work/rotated.log" \
	"SAFE_OWNER_KEYS holds the key of $(address_of "$signer_key"), which is not an owner of CLAIM_SIGNER $signer_safe" \
	SAFE_OWNER_KEYS="${signer_owner_keys[0]},$signer_key"
expect_orchestrator_refusal "$work/rotated.log" \
	"SAFE_OWNER_KEYS holds the key of $(address_of "${admin_owner_keys[0]}"), which is not an owner of CLAIM_SIGNER $signer_safe" \
	SAFE_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[1]}"
expect_orchestrator_refusal "$work/rotated.log" \
	"SAFE_OWNER_KEYS holds the key of $(address_of "${signer_owner_keys[0]}") twice" \
	SAFE_OWNER_KEYS="${signer_owner_keys[0]},${signer_owner_keys[0]}"
expect_orchestrator_refusal "$work/rotated.log" \
	"SIGNER_PRIVATE_KEY and SAFE_OWNER_KEYS are both set" \
	SIGNER_PRIVATE_KEY="$signer_key" SAFE_OWNER_KEYS="$two_owner_keys"

# The website's claimer: a fresh key anvil gives ETH, which the proofs' coupons
# for the website pay.
claimer_key=$(fresh_key)
claimer=$(address_of "$claimer_key")
cast rpc anvil_setBalance "$claimer" 0xDE0B6B3A7640000 --rpc-url "$fork" >/dev/null

step "Proving the Safe admin and the Safe coupon signer on the fork"
ORDER_HASH=$new_order_hash PREVIOUS_ORDER_HASH=$order_hash REHEARSAL_WEBSITE_CLAIMER=$claimer \
	PUBLIC_CLAIM_INTERPRETER=$interpreter PUBLIC_CLAIM_STORE=$store \
	PUBLIC_CLAIM_BYTECODE=$new_bytecode PUBLIC_CLAIM_INPUT_TOKEN=$input_token \
	ADMIN_SAFE=$admin_safe SIGNER_SAFE=$signer_safe REHEARSAL_MIN_LOCK_AMOUNT=$min_lock_amount \
	REHEARSAL_SIGNER_OWNER_KEYS="$two_owner_keys" \
	REHEARSAL_SIGNER_ONE_OWNER_KEY="${signer_owner_keys[0]}" \
	REHEARSAL_ADMIN_OWNER_KEYS="${admin_owner_keys[0]},${admin_owner_keys[1]}" \
	proofs --match-contract SafeSignerRehearsal | tee "$work/safe-proofs.log"
coupon_value() { grep -oE "$1=[^ ]+" "$work/safe-proofs.log" | head -1 | cut -d= -f2- || true; }
safe_coupon=$(coupon_value SAFE_COUPON)
one_owner_coupon=$(coupon_value ONE_OWNER_COUPON)
non_owner_coupon=$(coupon_value NON_OWNER_COUPON)
claim_calldata=$(coupon_value CLAIM_CALLDATA)
[[ -n $safe_coupon && -n $one_owner_coupon && -n $non_owner_coupon && -n $claim_calldata ]] ||
	fail "the proofs printed no coupons for the website"

step "The website's coupon status reads the Safe-signed coupon on the fork, before and after its claim"
# The website's build variables are the rotated deploy record's.
mapfile -t website < <(sed -n '/website:/,$p' "$work/rotated.log" | grep -oE 'PUBLIC_[A-Z_]+=[^ ]+')
# A lock funds the claim: anvil gives the locker 100 HOT (HOT keeps balances in
# its storage slot 2).
locker=$(address_of "${keys[3]}")
hot=0x6c6EE5e31d828De241282B9606C8e98Ea48526E2
cast rpc anvil_setStorageAt "$hot" "$(cast index address "$locker" 2)" \
	"$(cast to-uint256 100000000000000000000)" --rpc-url "$fork" >/dev/null
[[ $(cast call --rpc-url "$fork" "$hot" 'balanceOf(address)(uint256)' "$locker" | cut -d' ' -f1) == 100000000000000000000 ]] ||
	fail "anvil did not give the locker HOT"
# The website reads at the `safe` block, which anvil answers as 32 blocks behind
# the latest, as Ethereum's lags: mining past that puts the rotation, and later
# the claim, in the block it reads.
mine_past_safe() { cast rpc anvil_mine 40 --rpc-url "$fork" >/dev/null; }
coupon_status() {
	mine_past_safe
	(
		cd ui
		env "${website[@]}" REHEARSAL_RPC_URL="$fork" REHEARSAL_SAFE_COUPON_STATUS="$1" \
			SAFE_COUPON="$safe_coupon" ONE_OWNER_COUPON="$one_owner_coupon" NON_OWNER_COUPON="$non_owner_coupon" \
			npx vitest run src/routes/api/coupon-status/server.fork.test.ts
	)
}
coupon_status unredeemed
# The claim as the website sends it, from the claimer; the vault holds the HOT a
# lock put there through the proofs' own fork, not this one, so lock first.
cast send --rpc-url "$fork" --private-key "${keys[3]}" "$hot" 'approve(address,uint256)' "$vault" 100000000000000000000 >/dev/null
cast send --rpc-url "$fork" --private-key "${keys[3]}" "$vault" 'lock(uint256,bytes32)' 100000000000000000000 \
	"$(cast keccak "a Unyt agent")" >/dev/null
cast send --rpc-url "$fork" --private-key "$claimer_key" 0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20 "$claim_calldata" >/dev/null ||
	fail "the Safe-signed coupon's claim failed on the fork"
echo "claimed the Safe-signed coupon on the fork"
coupon_status redeemed

step "Deploying with UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes: a key as the vault admin, the signer Safe as the coupon signer"
deploy_block=$(next_block)
ETH_RPC_URL=$fork UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes ADMIN_ADDRESS=$admin VALID_SIGNER=$signer_safe \
	./deploy-mainnet.sh "${wallet[@]}" | tee "$work/unsafe.log"
grep -qF "UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes: ADMIN_ADDRESS $admin is one key, which alone controls the vault" \
	"$work/unsafe.log" || fail "the deploy did not log the override"
read_record "$work/unsafe.log"
[[ $(record_value PUBLIC_CLAIM_SIGNER "$work/unsafe.log") == "$signer_safe" ]] ||
	fail "the deploy record does not name the signer Safe"
expect_startup "$work/unsafe.log" "for the signer Safe as the coupon signer from the deploy" \
	SAFE_OWNER_KEYS="$two_owner_keys" UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes
ORDER_HASH=$order_hash PUBLIC_CLAIM_INTERPRETER=$interpreter PUBLIC_CLAIM_STORE=$store \
	PUBLIC_CLAIM_BYTECODE=$bytecode PUBLIC_CLAIM_INPUT_TOKEN=$input_token \
	ADMIN_ADDRESS=$admin SIGNER_SAFE=$signer_safe HOLO_BRIDGE_DEPLOYER=$bridge_deployer \
	REHEARSAL_DEPLOY_BLOCK=$deploy_block REHEARSAL_SIGNER_OWNER_KEYS="$two_owner_keys" \
	proofs --match-contract SingleKeyAdminRehearsal

step "Fork rehearsal passed"
