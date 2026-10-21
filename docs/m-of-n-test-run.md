# M-of-N test run on Base Sepolia

How to repeat the by-hand run of 2026-10-20: a second TestNet on the real Base Sepolia
whose 2-of-3 Safe is both the vault admin and the coupon signer, three mock signer
services, lock and claim on a local website. No Holochain, no Unyt. Nothing here
touches Unyt's TestNet or the public website.

Steps 1-8 follow the guide "Try M-of-N on Base Sepolia by hand" in DEPLOY.md; this file
adds what that guide does not have: recovery from RPC lag, the faucet, the failover
test with `make-coupon`, and coupons with bad signatures, in the website and on chain.

## The deploy from 2026-10-20

| | Address |
|---|---|
| Mock HOT (`TOKEN_ADDRESS`) | `0x48c47491e4B424896a9FB9F23ff8119B67DADff4` |
| Safe 1.4.1, 2 of 3 (`ADMIN_ADDRESS` = `VALID_SIGNER` = `CLAIM_SIGNER`) | `0x66338910D819897450c694d7944744E02b2dcfAb` |
| Owners | `0x325965c0C90D6fa4ea5bb15D66c4DD490269b7e4`, `0xD79eB17Fc942D12101fD219A3De5e84a23741E8d`, `0xBD29c94F123F2786b12A9e3f8c112ffa98842c6e` |
| HoloBridgeDeployer (block 48367313) | `0x6D66381eae230884260254D623c90EB45F7032f1` |
| HoloLockVault | `0xEdDdBEF1b6dEb820888661829ecF70e9621328B4` |
| Claim input token | `0xcd93874448155B54FbA986bd0081836956623278` |
| Claim order hash | `0x4ef57a4b145680474a56c7ff38c2b1d65e851eb0b76116efb0afa3098277ca61` |

Safe app: https://app.safe.global/home?safe=basesep:0x66338910D819897450c694d7944744E02b2dcfAb

## Before you start

- Foundry (`forge`, `cast`): `curl -L https://foundry.paradigm.xyz | bash`, then `foundryup`.
  Not `apt install forge`, which is another tool. Also `jq`, `node`, and `cargo` if you
  run the orchestrator checks.
- A `deployer` keystore with some Base Sepolia ETH (the whole deploy used < 0.0001 ETH).
- Run everything in **one terminal**. The steps reuse shell variables (`REPO`, `OWNERS`,
  `SERVICES`, and everything from `.env`). In a new terminal, set them again (see
  "New terminal" below).

## 1. Worktree

```bash
git fetch origin
git worktree add ../raindex-orders-mofn origin/safe-admin-and-signer
cd ../raindex-orders-mofn
forge soldeer install && npm ci && (cd ui && npm ci) && (cd bridge-orchestrator && cargo build)
cp .env.example .env
REPO=$PWD
```

A worktree of its own keeps Unyt's `.env`, `ui/.env.development.local` and
`broadcast/.../84532/run-latest.json` in your main checkout untouched.

## 2. Three owner keys

Fresh keys, not your MetaMask address (that is an EIP-7702 account with code; the
deploy refuses a contract as owner).

```bash
mkdir -m 700 ~/mofn
for i in 1 2 3; do cast wallet new --json | jq -r '.data[0] | .address, .private_key' > ~/mofn/owner$i; done
chmod 600 ~/mofn/owner*
OWNERS=$(for i in 1 2 3; do head -1 ~/mofn/owner$i; done | paste -sd,)
{ sed -n 2p ~/mofn/owner1; sed -n 2p ~/mofn/owner2; } > ~/mofn/two-keys && chmod 600 ~/mofn/two-keys
```

`~/mofn/ownerN`: line 1 the address, line 2 the key.

## 3. Deploy

```bash
SAFE_OWNERS=$OWNERS SAFE_THRESHOLD=2 ./deploy-base-sepolia.sh all-safe --account deployer | tee ~/mofn/deploy.log
set -a; source .env; set +a
```

Mock HOT, a mint of 1000 to the deployer, the Safe, then vault and claim order in one
transaction. Every step asks for the keystore password.

### If it stops part-way

`sepolia.base.org` is load-balanced, so a read right after a deploy can reach a node
that has not seen it yet. Since `5e619ed` the script waits for each contract's code;
before that, two things failed on 10-20:

- **"call to non-contract address" at the mint:** the token is deployed and its address
  is in `.env`. Run the rest one step at a time:
  ```bash
  ./deploy-base-sepolia.sh mint --account deployer
  SAFE_OWNERS=$OWNERS SAFE_THRESHOLD=2 ./deploy-base-sepolia.sh safe --account deployer
  ./deploy-base-sepolia.sh bridge --account deployer
  ```
  A wrong password fails only that step; the mint can be skipped (step 7 mints to MetaMask).
- **"pass the HoloBridgeDeployer the deploy printed, not the vault" after the bridge
  deploy:** the deploy landed, only its read-back failed, so `.env` has no vault or
  order yet. Read it back again (read-only, safe to repeat) and write the values:
  ```bash
  forge script script/DeployBridge.s.sol:DeployBaseSepoliaBridge --rpc-url https://sepolia.base.org \
    --sig 'record(address,uint256)' <HoloBridgeDeployer> <block> | tee ~/mofn/deploy.log
  for k in BASE_SEPOLIA_LOCK_VAULT_ADDRESS CLAIM_INPUT_TOKEN ORDER_HASH ORDER_OWNER CLAIM_SIGNER CLAIM_BYTECODE; do
    v=$(grep -oP "^\s*$k=\K0x[0-9a-fA-F]+" ~/mofn/deploy.log | head -1)
    [ -n "$v" ] && sed -i "/^$k=/d" .env && echo "$k=$v" >> .env || echo "MISSING $k"
  done
  set -a; source .env; set +a
  echo $ORDER_HASH
  ```
  The script's error line prints `<HoloBridgeDeployer>` and `<block>`.

A dedicated RPC (Alchemy, Infura) in `BASE_SEPOLIA_RPC_URL` avoids the lag.

## 4. Website settings

```bash
{ sed -n '/website:/,$p' ~/mofn/deploy.log | grep -oE 'PUBLIC_[A-Z_]+=[^ ]+'; echo BASE_SEPOLIA_RPC_URL=https://sepolia.base.org; } > ui/.env.development.local
```

## 5. Three mock signers

```bash
for i in 1 2 3; do
  OWNER_KEY=$(sed -n 2p ~/mofn/owner$i) SAFE=$ADMIN_ADDRESS CHAIN_ID=84532 PORT=1860$i \
  ORDER_HASH=$ORDER_HASH ORDER_OWNER=$ORDER_OWNER ORDERBOOK_ADDRESS=$ORDERBOOK_ADDRESS \
  TOKEN_ADDRESS=$TOKEN_ADDRESS VAULT_ID=$VAULT_ID SIGNED_RECORD=$HOME/mofn/signed$i.json \
  node ui/scripts/mock-signer-service.mjs > ~/mofn/service$i.log 2>&1 & echo $! > ~/mofn/service$i.pid
done
SERVICES=$(for i in 1 2 3; do echo "$(head -1 ~/mofn/owner$i)=http://127.0.0.1:1860$i"; done | paste -sd,)
```

Check they answer: `for i in 1 2 3; do curl -s http://127.0.0.1:1860$i/signer; echo; done`

They stop when their terminal closes. The mock checks the coupon's words against the
deploy, but nothing on Holochain.

### New terminal

```bash
cd <your worktree> && REPO=$PWD
set -a; source .env; set +a
OWNERS=$(for i in 1 2 3; do head -1 ~/mofn/owner$i; done | paste -sd,)
SERVICES=$(for i in 1 2 3; do echo "$(head -1 ~/mofn/owner$i)=http://127.0.0.1:1860$i"; done | paste -sd,)
```

With `SERVICES` empty, `make-coupon` says "the claim order takes coupons from <Safe>,
not the test signer ..." - that means it got no owners, not that the coupon is wrong.

## 6. Orchestrator startup checks (optional, skipped on 10-20)

See DEPLOY.md step 6. It asks each service who it signs for and has the claim order
take a probe signed by 2 owners and refuse it signed by 1.

## 7. Mock HOT, lock, coupon, claim

Mock HOT's `mint` is open to anyone:

```bash
cast send $TOKEN_ADDRESS 'mint(address,uint256)' <MetaMask address> 1000000000000000000000 \
  --account deployer --rpc-url https://sepolia.base.org
```

Or the faucet page: it **sends** from its wallet (1000 per request, once per 10 min per
address), it does not mint. Put `FAUCET_PRIVATE_KEY=<key>` in `ui/.env.development.local`,
mint mock HOT to that wallet's address, and give it a little Base Sepolia ETH.

Second terminal: `cd ui && npm run dev`, open http://localhost:5173 with MetaMask on
Base Sepolia, lock on `/lock`. Then, first terminal:

```bash
cd ui
SAFE_OWNER_SERVICES=$SERVICES npm run make-coupon -- --recipient <MetaMask address> --amount 5
# or with owner keys from a file instead of the services:
npm run make-coupon -- --recipient <MetaMask address> --amount 5 --owner-keys-file ~/mofn/two-keys
```

`make-coupon` reads threshold and owners from the Safe, asks each service, checks each
signature, sorts them by owner, and has the Safe's `isValidSignature` accept them before
it prints the claim link. Open it, "Claim HOT". At the claim, Raindex asks the Safe the
same question. A claim pays out of what was locked: keep the coupons' total below it.

**Result 10-20: claimed.**

## 8. Failover: one signer down, two down

```bash
kill $(cat ~/mofn/service3.pid)
SAFE_OWNER_SERVICES=$SERVICES npm run make-coupon -- --recipient <MetaMask address> --amount 2
# owner 3 did not answer, owners 1 and 2 sign: a link. Claim it.

kill $(cat ~/mofn/service2.pid)
SAFE_OWNER_SERVICES=$SERVICES npm run make-coupon -- --recipient <MetaMask address> --amount 2
# "1 of the Safe's owners signed, and its threshold is 2: ..." and no link.

kill $(cat ~/mofn/service1.pid)
```

**Result 10-20: two down refused, as expected.** Restart with step 5.

`make-coupon` refuses below the threshold itself, so this does not reach the chain.
Section 9 does.

## 9. Coupons with bad signatures (reach the Safe)

A coupon is `signer,signature,word0,...,word8`. For 2 of 3 the signature is two 65-byte
owner signatures, owner addresses ascending: `0x` and 260 hex digits, 130 per owner.
Take a valid coupon (all three services up), do not claim it, and change the signature:

```bash
SAFE_OWNER_SERVICES=$SERVICES npm run make-coupon -- --recipient <MetaMask address> --amount 3
C='<everything after "Coupon: ">'
IFS=, read -r SIGNER SIG REST <<< "$C"
echo ${#SIG}        # 262
S1=${SIG:2:130}; S2=${SIG:132:130}
S2BAD=${S2:0:10}$([ "${S2:10:1}" = 0 ] && echo 1 || echo 0)${S2:11}   # owner 2's, one digit changed
link() { echo "http://localhost:5173/claim?c=$SIGNER,0x$1,$REST"; }

link $S1              # one signature                 -> refused
link $S1$S2BAD        # two, one valid                -> refused
link $S1$S2BAD$S2BAD  # three, one valid              -> refused
link $S1$S1           # owner 1 twice                 -> refused
link $S2$S1           # both valid, wrong order       -> refused
link $S1$S2$S2BAD     # two valid, then junk          -> accepted
link $S1$S2           # the original                  -> accepted
```

All seven share one nonce, so only one of the two accepted ones can be claimed: try the
refused ones first, then claim one accepted link. The other then answers "already
claimed".

Before it sends, the claim page runs the exact `takeOrders4` call as an `eth_call`
against the orderbook. So a refusal there is already the claim order's own answer,
from the chain's state, not a separate check in the website: Raindex checks the signed
context with ERC-1271, the Safe's `isValidSignature` refuses, and the orderbook reverts
with `InvalidSignature`. The page shows that as "This coupon's signature does not match
its contents". `test/TestOneSafeAdminAndSigner.t.sol` has one owner's signature, an
owner and a stranger, and one owner twice as forge tests against the real Raindex.

`$S1$S2$S2BAD` is accepted because the Safe reads only its threshold's worth of
signatures, the first 2, and ignores the rest. Extra bytes add no signer.

**Result 10-20: the claim page refused the bad coupons.**

### The same coupons as mined transactions

To see a refused claim mined and reverted, not only refused by the `eth_call`, use the
branch `mofn-onchain-test`. It is this branch plus one commit, for testing only, never
merged: the claim page skips its check and sends `takeOrders4` with a fixed gas limit
of 1,000,000, so the wallet does not stop at gas estimation.

```bash
git fetch origin && git checkout --detach origin/mofn-onchain-test
# restart `npm run dev`, open a refused link from above, "Claim HOT"
```

MetaMask may warn that the transaction is likely to fail; confirm anyway. The
transaction is mined, reverts, and the page shows the revert reason it reads back
afterwards. On BaseScan (sepolia.basescan.org) the transaction shows "Fail" and no
token transfer. Check that the coupon still works: the valid link claims afterwards,
because a reverted claim uses no nonce. Go back with
`git checkout --detach origin/safe-admin-and-signer`.

**Result 10-20, on chain from MetaMask on `mofn-onchain-test`:** the coupons with fewer
than two valid owner signatures were mined and reverted; the coupon with two valid
signatures and a third, broken one claimed. MetaMask sent the failing claims. An earlier
"signature does not match" in the console was the dev server still running the PR
branch from another worktree: check `git log --oneline -1` in the worktree that runs
`npm run dev`.

## 10. The Safe in the Safe app

Import owner 1's and owner 2's keys into MetaMask (Add account, Import account,
`sed -n 2p ~/mofn/owner1`). Decline a smart account. Give the executing owner gas:
`cast send $(head -1 ~/mofn/owner2) --value 0.005ether --account deployer --rpc-url https://sepolia.base.org`.
Open the link at the top. You can look without connecting. Changing owners or threshold
needs 2 of the 3 to sign. That is how Unyt's notary keys replace these test keys later,
with no redeploy and no new claim order. Do not take the offer to update to Safe 1.5.0.

DEPLOY.md step 8 has a worked admin call (`setMinLockAmount`) signed by two owners.

## Don't

- Run `cf-deploy` from this worktree: the public site stays on Unyt's vault.
- Touch the main checkout's `.env`.
- Commit `broadcast/*/84532/run-latest.json` from the worktree.

## Clean up

```bash
for i in 1 2 3; do kill $(cat ~/mofn/service$i.pid) 2>/dev/null; done
# keep ~/mofn while the Safe's owners are these test keys
```
