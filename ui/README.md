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

## Pausing the bridge

While a Worker's `BRIDGE_PAUSED` secret is `true`, its Lock and faucet pages show a stop page, `/api/faucet` answers `503`, and an open Lock page sends nothing. Claim works as usual. Any other value, or none, leaves the site open.

Run these from `ui/`. Each deploys a new version of the Worker at once, with no build, and no deploy removes the secret.

| Worker                            | Pause                                                 | Resume                                                   |
| --------------------------------- | ----------------------------------------------------- | -------------------------------------------------------- |
| `hot-bridge-ui` (TestNet)         | `npx wrangler secret put BRIDGE_PAUSED`               | `npx wrangler secret delete BRIDGE_PAUSED`               |
| `hot-bridge-ui-mainnet` (MainNet) | `npx wrangler secret put BRIDGE_PAUSED --env mainnet` | `npx wrangler secret delete BRIDGE_PAUSED --env mainnet` |

`secret put` asks for the value: enter `true`. `GET /api/status` answers `{"paused": true}` once the pause is live.
