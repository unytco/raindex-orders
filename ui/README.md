# create-svelte

Everything you need to build a Svelte project, powered by [`create-svelte`](https://github.com/sveltejs/kit/tree/main/packages/create-svelte).

## Creating a project

If you're seeing this, you've probably already done this step. Congrats!

```bash
# create a new project in the current directory
npm create svelte@latest

# create a new project in my-app
npm create svelte@latest my-app
```

## Developing

Once you've created a project and installed dependencies with `npm install` (or `pnpm install` or `yarn`), start a development server:

```bash
npm run dev

# or start the server and open the app in a new browser tab
npm run dev -- --open
```

## Building

To create a production version of your app:

```bash
npm run build
```

You can preview the production build with `npm run preview`.

> To deploy your app, you may need to install an [adapter](https://kit.svelte.dev/docs/adapters) for your target environment.

## Networks and the orderbook ABI

`PUBLIC_NETWORK` is `base-sepolia` (the TestNet, chain 84532) or `mainnet`; the build variables and their TestNet defaults are in `src/lib/network.ts`, and [DEPLOY.md](../DEPLOY.md#website) names them. `src/generated.ts` holds the Raindex V6 ABI from `forge inspect IRaindexV6 abi --json`, plus the errors a claim can revert with; `scripts/coupon-fixtures.mjs` writes the coupon-status test fixtures.

## Pausing the bridge

While a Worker's `BRIDGE_PAUSED` secret is exactly `true`, every page shows a stop page, the TestNet `/api/faucet` answers `503`, and a Lock or Claim page left open refuses to send. Any other value, or none, leaves the site open.

Run these from `ui/`. Each deploys a new version of the Worker immediately, with no build. Later deploys keep the secret, but a rollback to a version from before the pause reopens the bridge.

| Worker                            | Pause                                                 | Resume                                                   |
| --------------------------------- | ----------------------------------------------------- | -------------------------------------------------------- |
| `hot-bridge-ui` (TestNet)         | `npx wrangler secret put BRIDGE_PAUSED`               | `npx wrangler secret delete BRIDGE_PAUSED`               |
| `hot-bridge-ui-mainnet` (MainNet) | `npx wrangler secret put BRIDGE_PAUSED --env mainnet` | `npx wrangler secret delete BRIDGE_PAUSED --env mainnet` |

`secret put` asks for the value: enter `true`. `GET /api/status` answers `{"paused":true}` once the pause is live.

The pause stops the website only. A lock or claim already in a wallet's prompt when the pause lands, or sent without the website, still lands, and the orchestrator bridges the lock.
