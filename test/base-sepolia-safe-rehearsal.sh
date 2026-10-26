#!/usr/bin/env bash
# Rehearses a fresh TestNet deploy on an anvil fork of Base Sepolia, with one
# Safe as the vault admin and the coupon signer (DEPLOY.md, "Deploy the TestNet
# on Base Sepolia"):
#   test/base-sepolia-safe-rehearsal.sh
# deploy-base-sepolia.sh all-safe deploys mock HOT, creates the Safe through
# Safe's factory, and deploys the vault and claim order in one transaction. Then
# the orchestrator passes its startup checks against the fork with the Safe's
# owners at mock signer services, ui/scripts/make-coupon.mjs makes 2-of-3 coupons
# that claim on the fork, and the one-Safe proofs run on the fork.
# FORK_URL is the Base Sepolia RPC anvil forks. Every transaction goes to an
# anvil this script starts on 127.0.0.1, never to FORK_URL.
set -euo pipefail
cd "$(dirname "$0")/.."

FORK_URL=${FORK_URL:-https://sepolia.base.org}
ORCHESTRATOR=${ORCHESTRATOR:-bridge-orchestrator/target/debug/bridge-orchestrator}
PORT=${REHEARSAL_PORT:-18645}
fork=http://127.0.0.1:$PORT

unset ETH_RPC_URL BASE_SEPOLIA_RPC_URL NETWORK ADMIN_ADDRESS VALID_SIGNER ADMIN_SAFE_THRESHOLD ADMIN_SAFE_OWNERS \
	UNSAFE_ALLOW_SINGLE_KEY_ADMIN UNSAFE_ALLOW_ONE_PARTY_SIGNER SAFE_OWNERS SAFE_THRESHOLD SAFE_SALT_NONCE TOKEN_ADDRESS PRIVATE_KEY
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

[[ -x $ORCHESTRATOR ]] ||
	fail "build the orchestrator first: cargo build in bridge-orchestrator, on its pinned toolchain, or set ORCHESTRATOR"
ORCHESTRATOR=$(realpath "$ORCHESTRATOR")

step "Starting anvil: a fork of Base Sepolia on $fork"
cast chain-id --rpc-url "$fork" >/dev/null 2>&1 && fail "something already answers on $fork"
anvil --host 127.0.0.1 --port "$PORT" --silent --fork-url "$FORK_URL" &
pids+=("$!")
for _ in $(seq 120); do
	[[ $(cast rpc web3_clientVersion --rpc-url "$fork" 2>/dev/null) == '"anvil/'* ]] && break
	sleep 0.5
done
[[ $(cast chain-id --rpc-url "$fork") == 84532 ]] || fail "the fork is not Base Sepolia, chain 84532"

address_of() { cast wallet address --private-key "$1"; }
fresh_key() { cast wallet new --json | jq -r '.data[0].private_key'; }
# Anvil's account 0 carries an EIP-7702 delegation on Base Sepolia: on the fork it
# is a plain key again, with ETH, and signs from a keystore as on Base Sepolia.
deployer_key=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
deployer=$(address_of "$deployer_key")
cast rpc anvil_setCode "$deployer" 0x --rpc-url "$fork" >/dev/null
cast rpc anvil_setBalance "$deployer" 0x3635C9ADC5DEA00000 --rpc-url "$fork" >/dev/null
cast wallet import deployer --private-key "$deployer_key" --keystore-dir "$work/keys" \
	--unsafe-password rehearsal >/dev/null
wallet=(--keystore "$work/keys/deployer" --password rehearsal)
nonce() { cast nonce "$deployer" --rpc-url "$fork"; }

owner_keys=("$(fresh_key)" "$(fresh_key)" "$(fresh_key)")
owners=$(for key in "${owner_keys[@]}"; do address_of "$key"; done | paste -sd,)
owner() { address_of "${owner_keys[$1]}"; }

step "deploy-base-sepolia.sh all-safe: mock HOT, the Safe, and the bridge in one transaction"
# The env file is .env.example, as an operator starts from, pointed at the fork.
sed "s|^BASE_SEPOLIA_RPC_URL=.*|BASE_SEPOLIA_RPC_URL=$fork|" .env.example >"$work/deploy.env"
printf 'SAFE_OWNERS=%s\nSAFE_THRESHOLD=2\n' "$owners" >>"$work/deploy.env"
ENV_FILE=$work/deploy.env ./deploy-base-sepolia.sh all-safe "${wallet[@]}" | tee "$work/deploy.log"
# shellcheck source=/dev/null
source "$work/deploy.env"
safe=$ADMIN_ADDRESS
[[ -n $safe && $VALID_SIGNER == "$safe" && $CLAIM_SIGNER == "$safe" ]] ||
	fail "the env file does not name the Safe as ADMIN_ADDRESS, VALID_SIGNER and CLAIM_SIGNER"
[[ $(cast storage "$safe" 0 --rpc-url "$fork") == "$(cast to-uint256 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762)" ]] ||
	fail "the Safe does not run the SafeL2 1.4.1 singleton"
[[ $(cast call --rpc-url "$fork" "$safe" 'getThreshold()(uint256)') == 2 ]] || fail "the Safe's threshold is not 2"
[[ $(cast call --rpc-url "$fork" "$BASE_SEPOLIA_LOCK_VAULT_ADDRESS" 'admin()(address)') == "$safe" ]] ||
	fail "the vault's admin is not the Safe"
grep -qF "VALID_SIGNER is ADMIN_ADDRESS $safe: one Safe is the vault admin and the coupon signer" "$work/deploy.log" ||
	fail "the deploy did not say one Safe holds both roles"
run_file=$work/broadcast/DeployBridge.s.sol/84532/run-latest.json
[[ $(jq '[.transactions[]] | length' "$run_file") == 1 ]] || fail "the bridge deploy sent other than one transaction"
bridge_deployer=$(jq -r '.transactions[0].contractAddress' "$run_file")
deploy_block=$(printf '%d' "$(jq -r '.receipts[0].blockNumber' "$run_file")")
echo "the Safe $safe is the vault admin and the coupon signer, from block $deploy_block"
before=$(nonce)
ENV_FILE=$work/deploy.env ./deploy-base-sepolia.sh safe "${wallet[@]}" >"$work/again.log"
grep -qF "The Safe exists, with these owners and threshold. Nothing was sent: $safe" "$work/again.log" ||
	fail "the safe step did not find the Safe again"
[[ $(nonce) == "$before" ]] || fail "the safe step sent a transaction for a Safe that exists"
echo "found again, sending nothing: $safe"

step "Starting mock signer services for the Safe's three owners (no Holochain checks)"
service_port=$((PORT + 10))
service_pids=()
for i in 0 1 2; do
	env -i PATH="$PATH" OWNER_KEY="${owner_keys[$i]}" SAFE="$safe" CHAIN_ID=84532 PORT=$((service_port + i)) \
		ORDER_HASH="$ORDER_HASH" ORDER_OWNER="$ORDER_OWNER" ORDERBOOK_ADDRESS="$ORDERBOOK_ADDRESS" \
		TOKEN_ADDRESS="$TOKEN_ADDRESS" VAULT_ID="$VAULT_ID" \
		node ui/scripts/mock-signer-service.mjs >"$work/service-$i.log" 2>&1 &
	pids+=("$!")
	service_pids+=("$!")
done
for i in 0 1 2; do
	for _ in $(seq 40); do
		curl -sf "http://127.0.0.1:$((service_port + i))/signer" >/dev/null && break
		sleep 0.25
	done
done

# Starts `bridge-orchestrator run` against the fork with the deploy record and
# `settings`, with no Holochain node: it gets past its startup checks only to
# stop where it needs the node.
orchestrate() {
	local dir
	dir=$(mktemp -d -p "$work")
	printf 'keystore:\n  type: lair_server\n  connection_url: unix:///%s/no-lair?k=abc\ndb_sync_level: Full\n' \
		"$dir" >"$dir/conductor-config.yaml"
	(
		cd "$dir"
		env -i PATH="$PATH" NETWORK=base-sepolia BASE_SEPOLIA_RPC_URL="$fork" \
			BASE_SEPOLIA_LOCK_VAULT_ADDRESS="$BASE_SEPOLIA_LOCK_VAULT_ADDRESS" TOKEN_ADDRESS="$TOKEN_ADDRESS" \
			ORDER_HASH="$ORDER_HASH" ORDER_OWNER="$ORDER_OWNER" CLAIM_SIGNER="$CLAIM_SIGNER" \
			CLAIM_BYTECODE="$CLAIM_BYTECODE" CLAIM_INPUT_TOKEN="$CLAIM_INPUT_TOKEN" "$@" \
			HOLOCHAIN_BRIDGING_AGENT_PUBKEY=uhCAkYoBhEs3GyOWslej78VfMRmSSdc2TXsRQmqFn5b3v8jl58Kkj \
			DB_PATH="$dir/bridge.db" CONDUCTOR_CONFIG="$dir/conductor-config.yaml" \
			LAIR_PASSPHRASE_FILE="$dir/no-passphrase" "$ORCHESTRATOR" run 2>&1 || true
	)
}
expect_startup() {
	local what=$1
	shift
	orchestrate "$@" >"$work/orchestrator.log"
	grep -qF "startup checks passed" "$work/orchestrator.log" || {
		cat "$work/orchestrator.log"
		fail "the orchestrator did not pass its startup checks $what"
	}
	echo "passed its startup checks $what"
}

step "Starting the orchestrator against the fork"
expect_startup "with the Safe's 3 owners at their signer services" \
	SAFE_OWNER_SERVICES="$(owner 0)=http://127.0.0.1:$service_port,$(owner 1)=http://127.0.0.1:$((service_port + 1)),$(owner 2)=http://127.0.0.1:$((service_port + 2))"
grep -qF "the claim order took the probe signed by 2 owners of CLAIM_SIGNER $safe, and refused it signed by 1" \
	"$work/orchestrator.log" || fail "the orchestrator did not probe with 2 owner signatures and with 1"
expect_startup "with the keys of 2 of the Safe's owners" SAFE_OWNER_KEYS="${owner_keys[1]},${owner_keys[2]}"

step "make-coupon's Safe mode: 2 of 3 owners sign a coupon, which claims on the fork"
# The website's values, as the deploy record prints them, in a file of the form
# npm run dev reads (DEPLOY.md, "Try M-of-N on Base Sepolia by hand").
{
	sed -n '/website:/,$p' "$work/deploy.log" | grep -oE 'PUBLIC_[A-Z_]+=[^ ]+'
	echo "BASE_SEPOLIA_RPC_URL=$fork"
} >"$work/ui.env"
[[ $(grep -c '^PUBLIC_' "$work/ui.env") == 10 ]] || fail "the deploy record has no 10 website lines"
grep -qxF "PUBLIC_CLAIM_SIGNER=$safe" "$work/ui.env" || fail "the website's claim signer is not the Safe"
agent=0x$(printf 'a%.0s' {1..64})
cast send "$TOKEN_ADDRESS" 'approve(address,uint256)' "$BASE_SEPOLIA_LOCK_VAULT_ADDRESS" 100000000000000000000 \
	--private-key "$deployer_key" --rpc-url "$fork" >/dev/null
cast send "$BASE_SEPOLIA_LOCK_VAULT_ADDRESS" 'lock(uint256,bytes32)' 100000000000000000000 "$agent" \
	--private-key "$deployer_key" --rpc-url "$fork" >/dev/null
claimer_key=$(fresh_key)
claimer=$(address_of "$claimer_key")
cast rpc anvil_setBalance "$claimer" 0x8AC7230489E80000 --rpc-url "$fork" >/dev/null # 10 ETH
make_coupon() { (cd ui && node scripts/make-coupon.mjs --recipient "$claimer" "$@"); }
# Claims `coupon` as the website does: takeOrders4 for exactly its amount, sent by
# the claimer, with the order the website's values describe.
claim() {
	(cd ui && env -i PATH="$PATH" COUPON="$1" KEY="$claimer_key" RPC="$fork" UI_ENV="$work/ui.env" node --input-type=module -e '
		import { readFileSync } from "node:fs"
		import { createWalletClient, createPublicClient, http, parseAbi, numberToHex } from "viem"
		import { privateKeyToAccount } from "viem/accounts"
		const env = Object.fromEntries(readFileSync(process.env.UI_ENV, "utf8").trim().split("\n").map(l => l.split("=")))
		const [signer, signature, ...words] = process.env.COUPON.split(",")
		const context = words.map(w => numberToHex(BigInt(w), { size: 32 }))
		const vaultId = "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b"
		const order = {
			owner: env.PUBLIC_LOCK_VAULT_ADDRESS,
			evaluable: { interpreter: env.PUBLIC_CLAIM_INTERPRETER, store: env.PUBLIC_CLAIM_STORE, bytecode: env.PUBLIC_CLAIM_BYTECODE },
			validInputs: [{ token: env.PUBLIC_CLAIM_INPUT_TOKEN, vaultId }],
			validOutputs: [{ token: env.PUBLIC_TOKEN_ADDRESS, vaultId }],
			nonce: numberToHex(0n, { size: 32 })
		}
		const abi = parseAbi([
			"struct IOV2 { address token; bytes32 vaultId; }",
			"struct EvaluableV4 { address interpreter; address store; bytes bytecode; }",
			"struct OrderV4 { address owner; EvaluableV4 evaluable; IOV2[] validInputs; IOV2[] validOutputs; bytes32 nonce; }",
			"struct SignedContextV1 { address signer; bytes32[] context; bytes signature; }",
			"struct TakeOrderConfigV4 { OrderV4 order; uint256 inputIOIndex; uint256 outputIOIndex; SignedContextV1[] signedContext; }",
			"struct TakeOrdersConfigV5 { bytes32 minimumIO; bytes32 maximumIO; bytes32 maximumIORatio; bool IOIsInput; TakeOrderConfigV4[] orders; bytes data; }",
			"function takeOrders4(TakeOrdersConfigV5 config) returns (bytes32, bytes32)"
		])
		const config = {
			minimumIO: context[1], maximumIO: context[1],
			maximumIORatio: "0xffffffe200000000000000000000000000000000000000000000000000000001",
			IOIsInput: true,
			orders: [{ order, inputIOIndex: 0n, outputIOIndex: 0n, signedContext: [{ signer, context, signature }] }],
			data: "0x"
		}
		const account = privateKeyToAccount(process.env.KEY)
		const chain = { id: 84532, name: "fork", nativeCurrency: { name: "ETH", symbol: "ETH", decimals: 18 }, rpcUrls: { default: { http: [process.env.RPC] } } }
		const pub = createPublicClient({ chain, transport: http() })
		const wallet = createWalletClient({ account, chain, transport: http() })
		const { request } = await pub.simulateContract({ account, address: env.PUBLIC_ORDERBOOK_ADDRESS, abi, functionName: "takeOrders4", args: [config] })
		const receipt = await pub.waitForTransactionReceipt({ hash: await wallet.writeContract(request) })
		if (receipt.status !== "success") throw new Error("the claim reverted")
	')
}
hot_of() { cast call "$TOKEN_ADDRESS" 'balanceOf(address)(uint256)' "$1" --rpc-url "$fork" | cut -d' ' -f1; }
coupon_of() { sed -n 's/^Coupon: //p' "$1"; }

(umask 077 && printf '# two of the Safe'"'"'s owners\n%s\n%s\n' "${owner_keys[2]}" "${owner_keys[0]}" >"$work/owner-keys")
make_coupon --env "$work/ui.env" --amount 5 --owner-keys-file "$work/owner-keys" >"$work/coupon-keys.log" ||
	{ cat "$work/coupon-keys.log"; fail "make-coupon did not sign with 2 owner keys from a file"; }
grep -qF "The Safe $safe on chain 84532: 2 of 3 owners sign" "$work/coupon-keys.log" || fail "make-coupon did not read the Safe"
coupon=$(coupon_of "$work/coupon-keys.log")
[[ ${coupon%%,*} == "$safe" && $(tr ',' '\n' <<<"$coupon" | wc -l) == 11 ]] ||
	fail "the coupon does not name the Safe or has other than nine words"
claim "$coupon" || fail "the coupon two owner keys signed did not claim"
[[ $(hot_of "$claimer") == 5000000000000000000 ]] || fail "the claim did not pay 5 mock HOT"
echo "a coupon signed with 2 owner keys from a file claimed 5 mock HOT"
claim "$coupon" 2>"$work/again.err" && fail "the same coupon claimed twice"
grep -qF "Nonce already used" "$work/again.err" || { cat "$work/again.err"; fail "the second claim failed for another reason"; }
echo "and claims once"

# The deploy record itself (.env), and the owners at their mock signer services.
services_list="$(owner 0)=http://127.0.0.1:$service_port,$(owner 1)=http://127.0.0.1:$((service_port + 1)),$(owner 2)=http://127.0.0.1:$((service_port + 2))"
SAFE_OWNER_SERVICES=$services_list make_coupon --env "$work/deploy.env" --amount 7 >"$work/coupon-services.log" ||
	{ cat "$work/coupon-services.log"; fail "make-coupon did not sign at the signer services"; }
grep -qF "Withdrawal ID: u" "$work/coupon-services.log" || fail "make-coupon named no withdrawal to the services"
[[ $(grep -c ' (service 127.0.0.1:[0-9]*): signed$' "$work/coupon-services.log") == 3 ]] ||
	{ cat "$work/coupon-services.log"; fail "not every signer service signed"; }
claim "$(coupon_of "$work/coupon-services.log")" || fail "the coupon the signer services signed did not claim"
[[ $(hot_of "$claimer") == 12000000000000000000 ]] || fail "the claim did not pay 7 more mock HOT"
echo "a coupon from the deploy record, signed at the 3 signer services, claimed 7 mock HOT"

SAFE_OWNER_KEYS=${owner_keys[1]} make_coupon --env "$work/ui.env" --amount 1 >"$work/coupon-one.log" 2>&1 &&
	fail "make-coupon made a coupon with one owner's key"
grep -qF "1 of the Safe's owners signed, and its threshold is 2" "$work/coupon-one.log" ||
	{ cat "$work/coupon-one.log"; fail "make-coupon did not refuse one owner's key for its threshold"; }
SAFE_OWNER_KEYS="${owner_keys[1]},$(fresh_key)" make_coupon --env "$work/ui.env" --amount 1 >"$work/coupon-stranger.log" 2>&1 &&
	fail "make-coupon made a coupon with a stranger's key"
grep -qF "is not an owner of the Safe $safe" "$work/coupon-stranger.log" || fail "make-coupon did not refuse a stranger's key"
make_coupon --env "$work/ui.env" --amount 1 >"$work/coupon-none.log" 2>&1 && fail "make-coupon signed for the Safe with no owner"
grep -qF "not the test signer" "$work/coupon-none.log" || fail "make-coupon did not ask for the Safe's owners"
make_coupon --env "$work/ui.env" --amount 1 --owner-keys "${owner_keys[0]}" >"$work/coupon-argv.log" 2>&1 &&
	fail "make-coupon took an owner key on the command line"
grep -qF "never on the command line" "$work/coupon-argv.log" || fail "make-coupon did not refuse a key on the command line"
echo "refused: one owner's key, a stranger's, no owner, and a key on the command line"
mainnet_port=$((PORT + 20))
anvil --host 127.0.0.1 --port "$mainnet_port" --chain-id 1 --silent &
pids+=("$!")
for _ in $(seq 40); do cast chain-id --rpc-url "http://127.0.0.1:$mainnet_port" >/dev/null 2>&1 && break; sleep 0.25; done
make_coupon --env "$work/ui.env" --amount 1 --owner-keys-file "$work/owner-keys" --rpc "http://127.0.0.1:$mainnet_port" \
	>"$work/coupon-mainnet.log" 2>&1 && fail "make-coupon signed for chain 1"
grep -qF "is Ethereum mainnet: make-coupon never signs for mainnet" "$work/coupon-mainnet.log" ||
	{ cat "$work/coupon-mainnet.log"; fail "make-coupon did not refuse chain 1"; }
echo "refused: an RPC for chain 1"

step "The orchestrator with the Safe's signer services down, as DEPLOY.md's M-of-N steps try it"
kill "${service_pids[2]}"
wait "${service_pids[2]}" 2>/dev/null || true
expect_startup "with 1 of the 3 signer services down" SAFE_OWNER_SERVICES="$services_list"
grep -qF "the signer service for $(owner 2) at 127.0.0.1:$((service_port + 2))" "$work/orchestrator.log" ||
	{ cat "$work/orchestrator.log"; fail "the orchestrator did not name the service that is down"; }
grep -qF "the claim order took the probe signed by 2 owners of CLAIM_SIGNER $safe, and refused it signed by 1" \
	"$work/orchestrator.log" || fail "the orchestrator did not probe with 2 owner signatures and with 1"
kill "${service_pids[1]}"
wait "${service_pids[1]}" 2>/dev/null || true
orchestrate SAFE_OWNER_SERVICES="$services_list" >"$work/orchestrator.log"
grep -qF "startup checks passed" "$work/orchestrator.log" && fail "the orchestrator started with 2 of the 3 signer services down"
grep -qF "1 of the owners this orchestrator asks can sign now, and CLAIM_SIGNER $safe is a Safe of threshold 2" \
	"$work/orchestrator.log" || { cat "$work/orchestrator.log"; fail "the orchestrator did not refuse for want of owners"; }
echo "refused to start with 2 of the 3 signer services down"

step "Proving the TestNet on the fork: one Safe is the vault admin and the coupon signer"
REHEARSAL=true REHEARSAL_RPC_URL=$fork REHEARSAL_TOKEN=$TOKEN_ADDRESS \
	MAINNET_LOCK_VAULT_ADDRESS=$BASE_SEPOLIA_LOCK_VAULT_ADDRESS ORDER_HASH=$ORDER_HASH \
	PUBLIC_CLAIM_INTERPRETER=0x4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf \
	PUBLIC_CLAIM_STORE=0x1Aa775533E28B1D843e1A589034984E3a62005DC \
	PUBLIC_CLAIM_BYTECODE=$CLAIM_BYTECODE PUBLIC_CLAIM_INPUT_TOKEN=$CLAIM_INPUT_TOKEN \
	BRIDGE_SAFE=$safe HOLO_BRIDGE_DEPLOYER=$bridge_deployer REHEARSAL_DEPLOY_BLOCK=$deploy_block \
	REHEARSAL_DEPLOYER=$deployer REHEARSAL_SIGNER_KEY="${owner_keys[0]}" \
	REHEARSAL_BRIDGE_OWNER_KEYS="${owner_keys[2]},${owner_keys[0]}" \
	REHEARSAL_BRIDGE_ONE_OWNER_KEY="${owner_keys[1]}" \
	forge test --match-path test/rehearsal/MainnetForkRehearsal.t.sol --match-contract OneSafeRehearsal -vv

step "Base Sepolia fork rehearsal passed"
