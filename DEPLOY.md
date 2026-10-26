# Holo Bridge deployment guide

This guide covers deploying and testing the complete HOT <> bridged HOT bridge infrastructure on the TestNet, Base Sepolia, and [deploying it on Ethereum mainnet](#mainnet-deploy).

## Raindex V6 and Rainlang

The bridge runs on the Raindex V6 and Rainlang release Rain deployed: rain.orderbook 0.1.15 (its RaindexV6 and subparser are unchanged since 0.1.14, cut 2026-08-23) and rainlang 0.1.9 (cut 2026-08-25). Rain deploys each release through its deterministic deployer, so a contract has the same address on every chain it is on, and `src/Constants.sol` takes each address, and the hash of its code, from the frozen `src/generated/<tag>/` file of the Soldeer package (`raindex` 0.1.15, `rainlang` 0.2.6). Every script checks the code at each address before it sends anything.

| Contract | Address, on Ethereum mainnet and Base Sepolia |
|---|---|
| RaindexV6 | `0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20` |
| RaindexV6SubParser | `0x09Bc7AF266012F44fb41D8Bd682da931666605e1` |
| Rainlang expression deployer (parses the claim expression) | `0xb90c299a8321fbdd8D0F367e560f5341F840dA3F` |
| Rainlang interpreter | `0x4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf` |
| Rainlang store | `0x1Aa775533E28B1D843e1A589034984E3a62005DC` |

None of these is on Ethereum Sepolia, which is why the TestNet is Base Sepolia (chain 84532).

Raindex V6 counts every amount as a decimal float. The vault converts each lock losslessly (the wei as the coefficient, -18 as the exponent) and a coupon's amount word is that float. V6 takes an order only at a positive io-ratio, and pulls at least one unit of the order's input token from the taker, so the claim order's ratio is 1e-30 and its input token is a `ClaimInputToken`, deployed with the vault, whose transfers succeed and move nothing. A V6 order carries its parsed bytecode, so the deploy record prints `CLAIM_BYTECODE` in place of an expression address.

## Prerequisites

1. A deployer wallet with Base Sepolia ETH: a Ledger, or an encrypted keystore as below. Base lists faucets at [docs.base.org](https://docs.base.org/base-chain/tools/network-faucets); the Coinbase Developer Platform faucet and Alchemy's give Base Sepolia ETH directly, and Base's bridge moves Ethereum Sepolia ETH over
2. Foundry installed (`forge`, `cast` commands available), and the Soldeer packages: `forge soldeer install`
3. Rust toolchain (for bridge-orchestrator)
4. Node.js 20+ (for UI, and `npm ci` at the repo root for `compose-rainlang.mjs`)

## Deploy the TestNet on Base Sepolia

### 1. Set Up Environment

```bash
# Copy example env file. It holds addresses, never a private key.
# .env.base-sepolia.example instead has the deployed TestNet's addresses.
cp .env.example .env

# Keep the deployer key in an encrypted keystore, entered at a prompt
cast wallet import deployer --interactive
```

Every step that sends a transaction takes the deployer as `--account deployer`, or `--ledger` for a Ledger. `deploy-base-sepolia.sh` refuses a `PRIVATE_KEY` in `.env` and a `--private-key` option. `BASE_SEPOLIA_RPC_URL` in `.env` is the RPC it sends through, `https://sepolia.base.org` unless set, and it refuses an RPC that answers for another chain.

### 2. Deploy All Contracts

The `deploy-base-sepolia.sh` script handles all deployment steps:

```bash
# Check your wallet and balance
./deploy-base-sepolia.sh status --account deployer

# Step 1: Deploy MockHOT token
./deploy-base-sepolia.sh token --account deployer
# Note: Updates .env with TOKEN_ADDRESS automatically

# Step 2: Deploy HoloLockVault, and the ClaimInputToken its claim order takes as input
./deploy-base-sepolia.sh vault --account deployer
# Note: Updates .env with BASE_SEPOLIA_LOCK_VAULT_ADDRESS and CLAIM_INPUT_TOKEN automatically

# Step 3: Mint test tokens to your wallet
./deploy-base-sepolia.sh mint --account deployer

# Step 4: Deploy claim order via HoloLockVault, accepting coupons from
# VALID_SIGNER (the test signer unless set)
./deploy-base-sepolia.sh order-via-vault --account deployer
# Note: Updates .env with ORDER_HASH and ORDER_OWNER automatically
```

### 3. Verify Deployment

```bash
# Show all deployed addresses and configuration
./deploy-base-sepolia.sh status
```

### 4. Run the orchestrator and the website against it

The orchestrator and the website take what is the same on every chain from TestNet's defaults (RaindexV6, the Rainlang interpreter and store, the vault ID, the test signer) and need the rest from this deploy: the orchestrator `BASE_SEPOLIA_LOCK_VAULT_ADDRESS`, `TOKEN_ADDRESS`, `ORDER_HASH`, `ORDER_OWNER`, `CLAIM_BYTECODE` and `CLAIM_INPUT_TOKEN`, and the website `PUBLIC_LOCK_VAULT_ADDRESS`, `PUBLIC_TOKEN_ADDRESS`, `PUBLIC_CLAIM_ORDER_HASH`, `PUBLIC_CLAIM_BYTECODE` and `PUBLIC_CLAIM_INPUT_TOKEN`. `rotate-claim-signer.sh record <block>` prints them all as a deploy record once the order is added, reading from `<block>` on (a block before the `order-via-vault` step); so does `cast` on the order's `AddOrderV3` event. `record` is written for a signer rotation and refuses while `ORDER_HASH`, the order a rotation replaces, is still on the orderbook. A first deploy has no previous order, so pass a zero hash, which overrides the one in `.env`:

```bash
NETWORK=base-sepolia VALID_SIGNER=0x8E72b7568738da52ca3DCd9b24E178127A4E7d37 \
  ORDER_HASH=0x0000000000000000000000000000000000000000000000000000000000000000 \
  ./rotate-claim-signer.sh record <block>
```

Then:

```bash
# bridge-orchestrator: NETWORK=base-sepolia, BASE_SEPOLIA_RPC_URL, SIGNER_PRIVATE_KEY
# (TEST_SIGNER_KEY in src/Constants.sol), HOLOCHAIN_BRIDGING_AGENT_PUBKEY and the lines above
cargo run -- run
# ui: PUBLIC_NETWORK=base-sepolia and the PUBLIC_* lines above, BASE_SEPOLIA_RPC_URL
# and FAUCET_PRIVATE_KEY as the Worker's secrets
npm run build
```

### Or: a fresh TestNet with one Safe as the vault admin and the coupon signer

This is the mainnet shape on Base Sepolia. `all-safe` deploys mock HOT, mints some to the wallet, creates the bridge's Safe (or finds it), and deploys the vault, its `ClaimInputToken` and the claim order in one `HoloBridgeDeployer` transaction, with the Safe as the vault admin from the first block and as the claim order's coupon signer:

```bash
SAFE_OWNERS=0x...,0x...,0x... SAFE_THRESHOLD=2 ./deploy-base-sepolia.sh all-safe --account deployer
```

The steps run one by one too: `token`, `mint`, `safe`, `bridge`.

- **`safe`** creates the Safe through Safe's own SafeProxyFactory 1.4.1, with the SafeL2 1.4.1 singleton (the one Safe's app uses on Base) and the CompatibilityFallbackHandler, which answers the claim order's EIP-1271 question. It refuses a threshold below 2 or of half the owners or fewer (every two sets of signers must share an owner, [docs/signer-service.md](./docs/signer-service.md#one-order-per-withdrawal)), an owner that is a contract, and an owner named twice, and checks the factory, singletons and handler are Safe's code, byte for byte. The address follows from the owners, the threshold and `SAFE_SALT_NONCE` (a fixed default unless set), so it is printed before anything is sent, and a second run with the same inputs finds the Safe and sends nothing. It writes `ADMIN_ADDRESS`, `ADMIN_SAFE_THRESHOLD`, `ADMIN_SAFE_OWNERS` and `VALID_SIGNER` to `.env`.
- **`bridge`** runs `script/DeployBridge.s.sol:DeployBaseSepoliaBridge`, the mainnet deploy's one transaction with `TOKEN_ADDRESS` as HOT, then reads the deploy record back from the chain and writes `BASE_SEPOLIA_LOCK_VAULT_ADDRESS`, `CLAIM_INPUT_TOKEN`, `ORDER_HASH`, `ORDER_OWNER`, `CLAIM_SIGNER` and `CLAIM_BYTECODE` to `.env`. It refuses what the mainnet deploy refuses (below), except the test signer.

The orchestrator then runs with `SAFE_OWNER_SERVICES`, the owners' signer services, or `SAFE_OWNER_KEYS`, owner keys it holds, in place of `SIGNER_PRIVATE_KEY` ([docs/signer-service.md](./docs/signer-service.md)). The website takes the record's values as for any new TestNet deploy, `PUBLIC_CLAIM_SIGNER` being the Safe.

`test/base-sepolia-safe-rehearsal.sh` runs `all-safe` on an anvil fork of Base Sepolia (`FORK_URL`, `https://sepolia.base.org` unless set), finds the Safe again without sending, starts the orchestrator against the fork with the owners at mock signer services and with owner keys, and with one and two of the services stopped, has `make-coupon` sign 2-of-3 coupons with owner keys from a file and at the services and claims them on the fork, and proves on the fork that two owner signatures claim, one or a stranger's do not, and the Safe's admin calls need two owners.

### Try M-of-N on Base Sepolia by hand

A second TestNet on the real Base Sepolia, whose 2-of-3 Safe is the vault admin and the coupon signer, next to the existing one. You start three mock signer services, watch the orchestrator's startup checks, lock on the website on your machine, and claim with a coupon two owners signed. Holochain is not needed. Nothing here touches the existing TestNet or the public website: it all happens in a worktree of its own, with its own `.env` and `ui/.env.development.local`. Never run `cf-deploy` from it; the public site stays on Unyt's vault.

**1. A worktree of its own.**

```bash
git fetch origin
git worktree add ../raindex-orders-mofn origin/safe-admin-and-signer
cd ../raindex-orders-mofn
forge soldeer install && npm ci && (cd ui && npm ci) && (cd bridge-orchestrator && cargo build)
cp .env.example .env
REPO=$PWD
```

The deploy writes `broadcast/*/84532/run-latest.json` here. Do not commit it.

**2. Three owner keys.** Make new ones. Your MetaMask address cannot be an owner: on Base Sepolia it is an EIP-7702 account, which has code, and the `safe` step refuses an owner that is a contract.

```bash
mkdir -m 700 ~/mofn
for i in 1 2 3; do cast wallet new --json | jq -r '.data[0] | .address, .private_key' > ~/mofn/owner$i; done
chmod 600 ~/mofn/owner*
OWNERS=$(for i in 1 2 3; do head -1 ~/mofn/owner$i; done | paste -sd,)
{ sed -n 2p ~/mofn/owner1; sed -n 2p ~/mofn/owner2; } > ~/mofn/two-keys && chmod 600 ~/mofn/two-keys
```

Each `~/mofn/ownerN` holds the owner's address, then its key. `two-keys` holds the keys of owners 1 and 2.

**3. Deploy.** The `deployer` keystore pays the gas, as for the existing TestNet:

```bash
SAFE_OWNERS=$OWNERS SAFE_THRESHOLD=2 ./deploy-base-sepolia.sh all-safe --account deployer | tee ~/mofn/deploy.log
set -a; source .env; set +a
```

This deploys mock HOT, mints 1000 to the deployer, creates the Safe (`ADMIN_ADDRESS`), and deploys the vault and the claim order in one transaction, with the Safe as admin and coupon signer. `.env` now holds the deploy record.

**4. The website's values**, from the record the deploy printed:

```bash
{ sed -n '/website:/,$p' ~/mofn/deploy.log | grep -oE 'PUBLIC_[A-Z_]+=[^ ]+'; echo BASE_SEPOLIA_RPC_URL=https://sepolia.base.org; } > ui/.env.development.local
```

**5. Three mock signer services**, one for each owner, with the record's values. They check the words of each coupon against the record, but nothing on Holochain:

```bash
for i in 1 2 3; do
  OWNER_KEY=$(sed -n 2p ~/mofn/owner$i) SAFE=$ADMIN_ADDRESS CHAIN_ID=84532 PORT=1860$i \
  ORDER_HASH=$ORDER_HASH ORDER_OWNER=$ORDER_OWNER ORDERBOOK_ADDRESS=$ORDERBOOK_ADDRESS \
  TOKEN_ADDRESS=$TOKEN_ADDRESS VAULT_ID=$VAULT_ID SIGNED_RECORD=$HOME/mofn/signed$i.json \
  node ui/scripts/mock-signer-service.mjs > ~/mofn/service$i.log 2>&1 & echo $! > ~/mofn/service$i.pid
done
SERVICES=$(for i in 1 2 3; do echo "$(head -1 ~/mofn/owner$i)=http://127.0.0.1:1860$i"; done | paste -sd,)
```

**6. The orchestrator's startup checks**, with no Holochain. It reads the Safe, asks each service who it signs for, and has the claim order take a probe coupon two owners signed and refuse it signed by one. Then it stops where it needs Holochain, which is expected here:

```bash
mkdir -p ~/mofn/orch
printf 'keystore:\n  type: lair_server\n  connection_url: unix:///%s/no-lair?k=abc\ndb_sync_level: Full\n' ~/mofn/orch > ~/mofn/orch/conductor-config.yaml
orchestrate() {
  (cd ~/mofn/orch && env -i PATH="$PATH" NETWORK=base-sepolia BASE_SEPOLIA_RPC_URL=https://sepolia.base.org \
    BASE_SEPOLIA_LOCK_VAULT_ADDRESS=$BASE_SEPOLIA_LOCK_VAULT_ADDRESS TOKEN_ADDRESS=$TOKEN_ADDRESS \
    ORDER_HASH=$ORDER_HASH ORDER_OWNER=$ORDER_OWNER CLAIM_SIGNER=$CLAIM_SIGNER \
    CLAIM_BYTECODE=$CLAIM_BYTECODE CLAIM_INPUT_TOKEN=$CLAIM_INPUT_TOKEN SAFE_OWNER_SERVICES=$SERVICES \
    HOLOCHAIN_BRIDGING_AGENT_PUBKEY=uhCAkYoBhEs3GyOWslej78VfMRmSSdc2TXsRQmqFn5b3v8jl58Kkj \
    DB_PATH=$HOME/mofn/orch/bridge.db CONDUCTOR_CONFIG=$HOME/mofn/orch/conductor-config.yaml \
    LAIR_PASSPHRASE_FILE=$HOME/mofn/orch/none "$REPO/bridge-orchestrator/target/debug/bridge-orchestrator" run)
}
orchestrate
```

Look for `the claim order took the probe signed by 2 owners of CLAIM_SIGNER <the Safe>, and refused it signed by 1` and `startup checks passed`. Then stop service 3 and run it again: it names the service that is down (`the signer service for <owner 3> at 127.0.0.1:18603 ...: the other owners sign until it answers`) and still passes, with owners 1 and 2. Stop service 2 too: it refuses to start, `1 of the owners this orchestrator asks can sign now, and CLAIM_SIGNER <the Safe> is a Safe of threshold 2`.

```bash
kill $(cat ~/mofn/service3.pid); orchestrate      # one service down: passes
kill $(cat ~/mofn/service2.pid); orchestrate      # two down: refuses
kill $(cat ~/mofn/service1.pid)
```

Then start the three again with step 5's loop.

**7. Lock and claim on the website on your machine.** Mock HOT mints to anyone, so give your MetaMask account some:

```bash
cast send $TOKEN_ADDRESS 'mint(address,uint256)' <your MetaMask address> 1000000000000000000000 --account deployer --rpc-url https://sepolia.base.org
```

In a second terminal, start the website: `cd ../raindex-orders-mofn/ui && npm run dev`, and open http://localhost:5173 with MetaMask on Base Sepolia. Lock some mock HOT on `/lock`. Then, back in the first terminal, make a coupon two owners sign, and open the claim link it prints:

```bash
cd ui
npm run make-coupon -- --recipient <your MetaMask address> --amount 5 --owner-keys-file ~/mofn/two-keys
SAFE_OWNER_SERVICES=$SERVICES npm run make-coupon -- --recipient <your MetaMask address> --amount 5
```

The first signs with the keys of owners 1 and 2, the second at the three services, which each sign a random withdrawal. `make-coupon` reads the Safe's threshold and owners from the chain, checks each signature, sorts them as the Safe wants, and has the Safe's `isValidSignature` accept them before it prints the link. It takes owner keys from a file or from `SAFE_OWNER_KEYS`, never from the command line, and refuses mainnet. One owner's key alone is refused: `1 of the Safe's owners signed, and its threshold is 2`. A claim pays out of what was locked.

**8. Manage the Safe in the Safe app.** The Safe app supports Base Sepolia and this Safe (SafeL2 1.4.1). Its own private-key login is off on Base Sepolia, so use MetaMask. In MetaMask, import owner 1's key (Add account, then Import account, and paste `sed -n 2p ~/mofn/owner1`), and do the same for owner 2. The owner that executes pays the gas: `cast send $(head -1 ~/mofn/owner2) --value 0.005ether --account deployer --rpc-url https://sepolia.base.org`. Open `https://app.safe.global/home?safe=basesep:<ADMIN_ADDRESS>` and connect as owner 1: it shows the three owners and the threshold, 2 of 3. Any admin call needs two of them. For example, to set the vault's minimum lock to 2 mock HOT: New transaction, Transaction Builder, the vault as the address, Custom data on, value 0, and as data the output of `cast calldata 'setMinLockAmount(uint256)' 2000000000000000000`. Sign as owner 1, switch MetaMask to owner 2, confirm and execute. `cast call $BASE_SEPOLIA_LOCK_VAULT_ADDRESS 'minLockAmount()(uint256)' --rpc-url https://sepolia.base.org` then answers 2000000000000000000. Do not take the app's offer to update the Safe to 1.5.0: the deploy and the rotation take only Safe 1.4.1.

### Deployed Contract Addresses (Base Sepolia)

| Contract | Address on Base Sepolia |
|----------|---------|
| Mock HOT | `0x916A830d1603252152fE17258bb10A393F173a72` |
| HoloLockVault | `0xF9ca23d23E92C3D7BD08AfCED901Df2Ddb173937` |
| ClaimInputToken | `0xfd7F9439C8f57Bf68619BcFFe0790b2628fa5928` |
| Claim order hash (owner: the vault) | `0x788f1663131e3010290f4f6995a5013a1ac557428b181f598c38d5e1be965ccb` |
| Claim order signer | the test signer |

Rain's contracts are at [their addresses above](#raindex-v6-and-rainlang). The test signer is `0x8E72b7568738da52ca3DCd9b24E178127A4E7d37`. `broadcast/*/84532/run-latest.json` records each transaction.

The previous TestNet, on Ethereum Sepolia against OrderBookV3 (vault `0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6`, orderbook `0xfca89cD12Ba1346b1ac570ed988AB43b812733fe`), does not carry over: its vault, order and coupons are V3's.

## Mainnet deploy

`deploy-mainnet.sh` deploys the bridge on Ethereum mainnet against the RaindexV6 Rain deployed there, in one transaction, with a Safe as the vault admin from that transaction on. It deploys no orderbook. At launch one Safe is both the vault admin and the coupon signer ([docs/enable-multisig.md](./docs/enable-multisig.md#one-safe-for-both-roles)).

First the Safe. `deploy-mainnet.sh safe` creates it through Safe's own SafeProxyFactory 1.4.1, with the Safe 1.4.1 singleton and the CompatibilityFallbackHandler, or finds it if it exists with those owners and threshold. Its address follows from the owners, the threshold and `SAFE_SALT_NONCE` (a fixed default unless set), so it is printed before anything is sent, and a second run sends nothing. It refuses a threshold below 2 or of half the owners or fewer (every two sets of signers must share an owner, [docs/signer-service.md](./docs/signer-service.md#one-order-per-withdrawal)), a contract or a repeated owner, and Safe contracts whose code is not Safe's release, and prints the inputs the deploy takes:

```bash
ETH_RPC_URL=<an Ethereum RPC> SAFE_OWNERS=0x...,0x...,0x... SAFE_THRESHOLD=2 \
./deploy-mainnet.sh safe --ledger       # or --account <keystore>
```

An existing Safe made in the Safe app works too, if it is Safe's SafeProxy 1.4.1 on the Safe 1.4.1 or SafeL2 1.4.1 singleton, with the CompatibilityFallbackHandler 1.4.1 and no module or guard (the deploy checks; [docs/enable-multisig.md](./docs/enable-multisig.md#create-the-safe)). Then the bridge:

```bash
npm ci   # compose-rainlang.mjs, which composes the claim expression
ETH_RPC_URL=<an Ethereum RPC> \
ADMIN_ADDRESS=<the Safe> \
ADMIN_SAFE_THRESHOLD=<its threshold> ADMIN_SAFE_OWNERS=<its owners, separated by commas> \
VALID_SIGNER=<the Safe; or a key, or another Safe> \
./deploy-mainnet.sh --ledger            # or --account <keystore>
```

- **Refusals.** Before it sends anything, it refuses a chain other than 1, a missing input, a raw key option such as `--private-key`, an `ADMIN_ADDRESS` equal to the deployer, a `VALID_SIGNER` equal to the test signer, a `VALID_SIGNER` with an EIP-7702 delegation (Raindex V6 asks a delegated key's delegate for a coupon's signature, never the key), a contract `VALID_SIGNER` that is not a Safe of threshold 2 to 20, a Safe `VALID_SIGNER` whose threshold is half its owners or fewer, a Safe `VALID_SIGNER` whose fallback handler does not hash coupons as the CompatibilityFallbackHandler does (it would refuse every coupon), a Safe `VALID_SIGNER` or `ADMIN_ADDRESS` that is not Safe's SafeProxy 1.4.1 on the Safe 1.4.1 or SafeL2 1.4.1 singleton with the CompatibilityFallbackHandler 1.4.1, or that has a module or a guard (a module acts as the Safe with no owner's signature), and a Rain contract that is not the release on the chain. It refuses an `ADMIN_ADDRESS` that is a key, a contract that is not a Safe, or a Safe of threshold 1, so that no one key controls the vault; `UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes` lets a key or a threshold-1 Safe through, and is logged. For a Safe admin it reads the Safe's threshold and owners, prints them, and refuses ones other than `ADMIN_SAFE_THRESHOLD` and `ADMIN_SAFE_OWNERS`: the operator states which Safe is meant, and a wrong address is caught before the vault goes to it.
- **What it sends.** One transaction: a `HoloBridgeDeployer`, whose constructor deploys the `ClaimInputToken`, then `HoloLockVault(HOT, RaindexV6, HOLO_VAULT_ID, <itself>, MIN_LOCK_AMOUNT)`, adds the claim order through the vault, and calls `setAdmin(ADMIN_ADDRESS)`. The deployer contract is the vault's admin only inside that transaction and holds no right after it; the key that sends it is never the admin. The transaction reverts as a whole if any step fails, leaving nothing behind.
- **The record.** It reads the vault from the `HoloBridgeDeployer`, reads back the vault's token, orderbook, vault ID and admin, checks that the vault's admin changed exactly once, from the deployer contract to `ADMIN_ADDRESS`, in the deploy's block, and reads the claim order. Then it prints the deploy record under the variable names the orchestrator and the website take, `CLAIM_BYTECODE` and `CLAIM_INPUT_TOKEN` among them. To print it again: `forge script script/DeployMainnet.s.sol:DeployMainnet --rpc-url $ETH_RPC_URL --sig 'record(address,uint256)' <HoloBridgeDeployer> <block>`.
- **The coupon signer.** The Safe: the orchestrator runs with `SAFE_OWNER_SERVICES`, each owner's signer service, which checks each withdrawal on its own Holochain node before it signs ([docs/signer-service.md](./docs/signer-service.md)). It can also hold owner keys itself, `SAFE_OWNER_KEYS`; holding M of them is one party signing as the Safe, and when the Safe is also the vault admin each such key also approves admin transactions. On mainnet the orchestrator refuses either unless `UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes`. A key: it runs with `SIGNER_PRIVATE_KEY`. When `VALID_SIGNER` is `ADMIN_ADDRESS`, the deploy logs that one Safe holds both roles.
- **Failure.** A run that sends its transaction and sees it revert, or not land, prints it. A new run deploys a new vault.

### Rehearse it first

`test/fork-rehearsal.sh` runs `deploy-mainnet.sh` and `rotate-claim-signer.sh`, unchanged, against an anvil fork of mainnet. Every transaction goes to an anvil it starts on 127.0.0.1.

```bash
nix develop -c test/fork-rehearsal.sh   # FORK_URL sets the mainnet RPC that anvil forks
```

First the launch shape. It creates the bridge's Safe, 2 of 3 fresh owner keys, with `deploy-mainnet.sh safe`, which refuses a threshold of 1 and, run again, finds the Safe and sends nothing. It creates three more Safes of anvil accounts and one with no fallback handler, then proves that the deploy sends nothing while it refuses a chain other than 1 (a plain anvil on chain 31337), a missing input, a raw key, the test signer, a signer with an EIP-7702 delegation (anvil's published accounts carry one on mainnet, so the rehearsal's coupon signers are fresh keys), a Safe of threshold 1 or a Safe with no fallback handler as the signer, a key or the deployer as the admin, a Safe of threshold 1, a contract that is not a Safe, and a stated threshold or owner list that is not the Safe's. Then it deploys with the bridge's Safe as `ADMIN_ADDRESS` and `VALID_SIGNER`, in one transaction, and starts mock signer services for the three owners (`ui/scripts/mock-signer-service.mjs`, which speaks [docs/signer-service.md](./docs/signer-service.md) and checks nothing on Holochain). The orchestrator refuses two owner keys and one key with one service unless `UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes` (those keys would be vault admin keys on the orchestrator's host), and passes its startup checks with two owner keys and the override, with the three services, with one service down, with one refusing, and with one key, one service and the override, each time with the probe signed by two owners taken and by one refused; it refuses one service left of two, a service signing with another key, a service answering for another owner, a key, and another Safe's owner keys. On the fork, a coupon with two owner signatures claims; one signature, an owner's and a stranger's, or one owner's twice fail; a nonce claims once; an expired coupon fails; a coupon's owner signatures execute no admin call; the Safe administers with two owners and not one; and the deployer, the deployer contract and each owner alone are refused every admin call.

Then the separate shape, on a second deploy: the admin Safe as the vault admin and a key as the coupon signer. The orchestrator passes its startup checks with the deploy record and the coupon signer's key, refuses another key, and refuses `SAFE_OWNER_KEYS` for a key signer. It proves that a lock lands in the vault, that a coupon signed as the orchestrator signs claims it back, that nonce reuse, a wrong signer and an expired coupon fail, that the vault's admin was handed over once, from the deployer contract to the Safe, in the deploy's block, that the deployer, the deployer contract and each owner alone are refused every admin call, and that the Safe acts with two owner signatures and not with one.

Then it moves the coupon signer to a 2 of 3 Safe through the admin Safe, as [docs/enable-multisig.md](./docs/enable-multisig.md) describes. The rotation refuses the test signer and a Safe of threshold 1, and its record refuses a signer the new order does not accept, a second order of the vault's left on the orderbook, and an order run by another interpreter. The orchestrator then refuses the keys of two of the Safe's three owners, which are one party, and passes its startup checks with them and `UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes`, through the Safe's own `isValidSignature`, and refuses a key for the Safe, one key, three, a key that is not an owner's, the admin Safe's owners, a key twice, and both key variables set. On the fork, a coupon with two owner signatures claims, and one with a single signature, with an owner's and a stranger's, with an owner's twice, or with the admin Safe's owners' fails. The website's coupon status reads a coupon the Safe's owners signed as unredeemed and two the Safe refuses as invalid, through the real Safe on the fork, then reads the first as redeemed once it is claimed (`ui/src/routes/api/coupon-status/server.fork.test.ts`). Last, a deploy with `UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes` hands the vault to a key, with the signer Safe as the coupon signer from the start.

`FORK_URL` is `https://ethereum-rpc.publicnode.com` unless set.

The rehearsal runs the orchestrator binary, so build it first: `cargo build` in `bridge-orchestrator`, on its pinned toolchain, or set `ORCHESTRATOR` to a built binary.

### Website

One SvelteKit build per network, each its own Cloudflare Worker: `hot-bridge-ui` for Base Sepolia, and `hot-bridge-ui-mainnet`, the wrangler env `mainnet`. Each Workers Builds project sets `PUBLIC_NETWORK` and the `PUBLIC_*` build variables the deploy record prints, and its RPC secret: `BASE_SEPOLIA_RPC_URL` and `FAUCET_PRIVATE_KEY`, or `ETH_RPC_URL`. `PUBLIC_NETWORK` defaults to `base-sepolia`, and a base-sepolia build takes TestNet's value, in `ui/src/lib/network.ts`, for what is the same on every chain (`PUBLIC_ORDERBOOK_ADDRESS`, `PUBLIC_CLAIM_INTERPRETER`, `PUBLIC_CLAIM_STORE`, `PUBLIC_CLAIM_SIGNER`), and for TestNet's deploy on Base Sepolia (`PUBLIC_TOKEN_ADDRESS`, `PUBLIC_LOCK_VAULT_ADDRESS`, `PUBLIC_CLAIM_INPUT_TOKEN`, `PUBLIC_CLAIM_ORDER_HASH`, `PUBLIC_CLAIM_BYTECODE`), and logs which. A build for a new TestNet deploy names all five: one given alone pairs with the old order's hash, which the order hash check refuses. A mainnet build takes none: it fails when a variable is missing or malformed. Either fails when the claim order the variables describe does not hash to `PUBLIC_CLAIM_ORDER_HASH`, and a mainnet build when the signer is the test signer. The MainNet Workers Builds project must set `PUBLIC_NETWORK=mainnet`. The build cannot check that `PUBLIC_CLAIM_SIGNER` is the order's signer, so take it from the deploy record. The mainnet website has no faucet.

The website waits for a sent transaction by polling every 2 seconds, Base's block time, for up to 5 minutes, as before: that is about 150 Base blocks, and 25 on mainnet.

## Testing the Complete Flow

### Test the website locally

`test/local-ui.sh` deploys the TestNet on an anvil fork of Base Sepolia on your machine, so you can lock mock HOT and claim it back in the website with MetaMask before deploying to Base Sepolia. Nothing goes to a live chain. It needs Foundry, and `npm ci` run at the top of the repo and in `ui/`.

```bash
test/local-ui.sh 0xYourMetaMaskAddress
```

It starts anvil on `127.0.0.1:8545`, forking `FORK_URL` (`https://sepolia.base.org` by default), so the chain ID stays 84532 and Raindex V6 and Rainlang are there. Then it runs `deploy-base-sepolia.sh all` (mock HOT, HoloLockVault, ClaimInputToken, and the claim order accepting the test signer's coupons) signed by anvil's published account 0, with its addresses in `.local-ui/deploy.env`, not your `.env`. It reads the deploy record back with `rotate-claim-signer.sh record`, gives your address 100 ETH and 10000 mock HOT, and writes `ui/.env.development.local`: the record's `PUBLIC_*` lines, `BASE_SEPOLIA_RPC_URL=http://127.0.0.1:8545`, and account 0 as the faucet's key. Only `npm run dev` reads that file; `npm run build` never does. Then it prints the next steps:

1. `cd ui && npm run dev`, and open http://localhost:5173.
2. In MetaMask, add a network with RPC URL `http://127.0.0.1:8545` and chain ID 84532 (or add that RPC URL to an existing Base Sepolia network and select it), and import the mock HOT token it prints.
3. Lock on `/lock`. Then make a coupon, from `ui/`, and open the claim link it prints:

   ```bash
   npm run make-coupon -- --recipient 0xYourMetaMaskAddress --amount 5
   ```

   `make-coupon` signs as the test signer, whose key is public in `src/Constants.sol`, for the claim order in `ui/.env.development.local` (or the env file `--env` names, a deploy record such as `.env` too). For a claim order whose signer is a Safe, the Safe's owners sign: owner keys from `--owner-keys-file` or `SAFE_OWNER_KEYS`, never the command line, or signer services in `SAFE_OWNER_SERVICES` ([Try M-of-N on Base Sepolia by hand](#try-m-of-n-on-base-sepolia-by-hand)). `--expiry <unix seconds>` and `--nonce <n>` override its defaults: a week from the chain's time, and a random nonce. It refuses mainnet (chain 1, `PUBLIC_NETWORK=mainnet`, or a mainnet record), and any chain but Base Sepolia. A claim pays out of what has been locked, so lock at least the coupon's amount first.
4. `test/local-ui.sh stop` stops anvil. Each start deploys afresh, so run the script again after a stop, and clear MetaMask's activity for the account (Settings > Advanced > Clear activity tab data), whose nonces are then stale.

### Lock Flow (HOT -> Bridged HOT)

1. **Start the bridge orchestrator:**
```bash
cd bridge-orchestrator
# Set HOLOCHAIN_BRIDGING_AGENT_PUBKEY, SIGNER_PRIVATE_KEY and the deploy record's
# values; what is the same on every chain defaults to TestNet's (bridge-orchestrator/README.md)
cargo run -- run
```

2. **Start the UI:**
```bash
cd ui
npm install
npm run dev
```

3. **Lock tokens:**
   - Open http://localhost:5173
   - Connect MetaMask to Base Sepolia
   - Select "Lock HOT -> bridged HOT" tab
   - Enter amount and Holochain agent public key
   - Approve and lock tokens
   - Watch the bridge-orchestrator detect the event and drive the Holochain bridge

### Claim Flow (Bridged HOT -> HOT)

1. **Coupon generation:**
   Withdrawal coupons are produced by the bridge orchestrator as part of its bridging cycle whenever a burn/withdraw request is observed on the Holochain side. See `bridge-orchestrator/src/signer.rs` for the signing logic.

2. **Claim via UI:**
   - Open http://localhost:5173/claim
   - Paste the coupon string into the input field
   - Or use URL: `http://localhost:5173/claim?c=<coupon>`
   - Click "Claim HOT"

3. **Claim via URL parameter:**
   - Coupons use a URL-safe format
   - Share: `http://localhost:5173/claim?c=<signer>,<signature>,<ctx0>,<ctx1>,...`

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                    SHARED LIQUIDITY POOL                         │
│         (Raindex V6 vault owned by HoloLockVault)               │
│                                                                  │
│   LOCK (HOT→bHOT)            HOT Tokens             CLAIM (bHOT→HOT)
│   ─────────────────►    ┌───────────────┐    ◄─────────────────
│   Deposits INTO         │   Balance: N   │         Withdraws FROM
│                         └───────────────┘                        │
└─────────────────────────────────────────────────────────────────┘
```

**Key Design**: The HoloLockVault contract owns both:
1. The vault where locked HOT is deposited
2. The claim order that allows withdrawals via signed coupons

This ensures LOCK deposits and CLAIM withdrawals operate on the **same pool of tokens**.

## Component Details

### HoloLockVault Contract (`src/HoloLockVault.sol`)

Functions:
- `lock(amount, holochainAgent)` - Lock tokens (deposited as a lossless decimal float), emit event for bridged HOT crediting
- `addOrder(config)` - Deploy claim order, an `OrderConfigV4` (admin only)
- `removeOrder(order)` - Remove claim order, an `OrderV4` (admin only)
- `adminWithdraw(amount, to)` - Emergency withdrawal (admin only)
- `vaultBalance()` - Check vault balance, in wei

### Bridge Orchestrator (`bridge-orchestrator/`)

Rust service that replaces the legacy `lock-watcher-rs` and `coupon-signer`:
```bash
cd bridge-orchestrator
cargo run -- run
```

Responsibilities:
- Polls the orderbook for new Lock events and drives the Holochain bridge
- Processes withdrawal requests from Holochain and generates signed claim coupons (see `CouponSigner::coupon` in `src/signer.rs`)
- Emits batched bridging RAVE transactions with explicit links and a coupons map
- Produces the same URL-safe coupon format consumed by the UI: `signer,signature,ctx0,ctx1,...,ctx8`

### UI (`ui/`)

SvelteKit web interface:
- `/` - Connect a wallet and see its network
- `/lock` - Lock HOT to receive bridged HOT
- `/claim` - Claim HOT with coupon
- `/claim?c=<coupon>` - Direct claim via URL parameter
- `POST /api/coupon-status`: the status of up to 20 claim coupons, read from the build's network through its RPC secret

#### What bounds coupon-status RPC use

- **Signature gate.** A coupon is read only if it names the claim order's signer (the order's `valid-signer`, `PUBLIC_CLAIM_SIGNER` in the build) and carries a signature that signer gives, checked as the orderbook checks it. A 65-byte signature must recover to the signer, and any other coupon of that form answers `invalid` and is never read. A signature of 2 to 20 owner parts, each a key's 65-byte signature, is a contract signer's, such as a Safe's: the signer's EIP-1271 `isValidSignature` checks it in the same `eth_call` that reads the nonces. Such a coupon causes a read even when it answers `invalid`, until a check finds that the claim signer has no code; for the next 10 minutes that Worker isolate answers such coupons `invalid` without a read, and logs that it does. A coupon signer Safe needs a threshold of 2 or more, as a 65-byte signature is read as a key's.
- **Cache.** Answers are kept in the Workers Cache, keyed by the coupon's signer, signature and context values, so a coupon respelled with other hex case or leading zeros shares its entry. `redeemed` and `expired` are kept from then on. `unredeemed` is kept for 60 s, and never once the coupon's expiry has passed. The Workers Cache is local to each Cloudflare data centre, so each data centre reads a coupon once for itself. A cache read or write that fails is logged and costs only a read; it never changes an answer.
- **One read at most.** A request makes at most one `eth_call` to its network's RPC secret, `BASE_SEPOLIA_RPC_URL` or `ETH_RPC_URL`: one Multicall3 `aggregate3` at the `safe` block, reading each uncached nonce once, at the key the claim order marks it at and at the key an order deployed before the nonce key changed marked it at (`hash(order-hash() nonce)`), so a coupon redeemed on either reads as redeemed. A request the cache answers in full, or with no valid coupon, makes none, and its `block` is `null`. An RPC that answers for another chain is refused.

A coupon whose expiry has passed, but which the `safe` block still shows unexpired, is read on every request until the `safe` block passes its expiry, about 10 to 15 minutes on Ethereum and a few minutes on Base.

#### Why 20 coupons per request

`hot-bridge-ui` runs on the Workers Free plan, which allows 50 subrequests per request, Cache API `match()` and `put()` calls included, and 10 ms of CPU. A request of N coupons the cache has not seen costs N cache matches, one RPC fetch and N cache puts. On a preview build, 6 to 20 new coupons answered `200`, 25 answered `500` (the 25th put), and 44 or 50 answered `502` (the RPC fetch). So a request takes at most 20 coupons: 41 subrequests and 20 signature checks. Checking a signature costs about 1 ms of CPU, and a coupon the cache holds is not checked again. The Workers Paid plan allows 10,000 subrequests per request.

#### Optional WAF rule

Optionally, for raw floods, add a Cloudflare WAF rate limiting rule in the dashboard (Security, then WAF, then Rate limiting rules) for the zone that serves `hot-bridge.unyt.dev`:

| Setting | Value |
|---|---|
| Match | URI path equals `/api/coupon-status` |
| Count by | IP |
| Rate | 20 requests per 10 s |
| Action | Block for 10 s |

An app sends about one request every 3 minutes, plus one each time Interaction Details is opened, so this sits far above genuine use. These are the Free plan's only period and timeout, and the Free plan cannot match on method. The rule covers the custom domain only, not the Worker's `workers.dev` and version URLs.

## Coupon Format

The signed coupon contains 9 context words, each a bytes32 the orchestrator writes as a decimal number:
| Index | Field | Description |
|-------|-------|-------------|
| 0 | recipient | Ethereum address to receive tokens, as a word |
| 1 | amount | The decimal float Raindex V6 pays out: the wei as the coefficient with exponent -18, so `0xffffffee` then the wei in the low 224 bits |
| 2 | expiry | Unix timestamp when coupon expires (a float of exponent 0 is its integer, as `block-timestamp()` is) |
| 3 | orderHash | Hash of the claim order (keccak256 of the abi-encoded OrderV4) |
| 4 | orderOwner | HoloLockVault address |
| 5 | orderbook | RaindexV6 address |
| 6 | outputToken | Token address (HOT) |
| 7 | outputVaultId | Vault ID |
| 8 | nonce | keccak256 of the 39 raw bytes of the withdrawal's Holochain transaction ID (prevents replay) |

The expression compares words 0 and 3 to 8 bit for bit, and words 1 and 2 as numbers. Each signer signs the EIP-191 hash of the packed words. `test/TestCouponSignature.t.sol` pins a test vector (fixed inputs, their nine words, the signature anvil's third account gives, the order hash and the claim's `takeOrders4` calldata) that `bridge-orchestrator/src/signer.rs`, `bridge-orchestrator/src/preflight.rs` and `ui/src/lib/coupon.test.ts`, `network.test.ts` and `claim.test.ts` reproduce.

## Troubleshooting

### "Order not found" in UI
The UI reads order status directly from the blockchain via RPC. If you see this error:
1. Verify the order was deployed: `./deploy-base-sepolia.sh status`
2. Check ORDER_HASH in `.env` matches the deployed order
3. Check the website's `PUBLIC_*` build variables against the deploy record

### Transaction fails with "Wrong signer"
The claim order does not accept the coupon's signer, as for a coupon signed before `rotate-claim-signer.sh` moved the order to a new signer. The orchestrator signs only with a key the order accepts: it refuses to start otherwise.

### "Nonce already used"
What it means depends on when the coupon was signed. Coupons signed before the per-withdrawal nonce carry the unix second they were signed in, a ten-digit nonce.

- **Ten-digit nonce:** coupons signed in the same second shared that nonce, so the error can mean a sibling coupon was claimed and this withdrawal is still unpaid. `POST /api/coupon-status` reports the coupon as `redeemed` in both cases. Re-issue the withdrawal (UNYT-1041).
- **Any other nonce:** the withdrawal has been claimed. Every coupon signed for one withdrawal carries the same nonce, so only one of them can be claimed.

## Security Notes

- Rotate the TestNet RPC API key once the faucet fix (UNYT-1040) is deployed: until then `/api/faucet` answered a failed RPC call with an error that held the full RPC URL. Change the `BASE_SEPOLIA_RPC_URL` Workers Builds build variable, which `ui/scripts/cf-deploy.sh` re-applies as the runtime secret on every deploy.
- Keep private keys out of `.env` files and command lines: sign with `--account` or `--ledger`
- The test signer key in this repo is for testing only, and every mainnet input refuses it
- Who holds the coupon signer and the vault admin, and how each moves to a multisig: [docs/enable-multisig.md](./docs/enable-multisig.md)
- The vault admin controls emergency withdrawals. On mainnet it is a Safe multisig from the deploy's first transaction, whose key holders follow [docs/key-holder.md](./docs/key-holder.md)
