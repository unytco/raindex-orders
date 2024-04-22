import { writable } from 'svelte/store'

// Define the initial config value
const initialConfig = {}

export const configStore = writable(initialConfig)
