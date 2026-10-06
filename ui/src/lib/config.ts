import * as publicEnv from '$env/static/public'
import { parseBridgeConfig } from './network'

/** This build's network values, from the build variables inlined into it. */
export const bridge = parseBridgeConfig(publicEnv as Partial<Record<string, string>>)

export const explorerTx = (hash: string) => `${bridge.explorer}/tx/${hash}`
export const explorerAddress = (address: string) => `${bridge.explorer}/address/${address}`
