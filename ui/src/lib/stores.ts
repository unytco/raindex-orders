import { writable } from 'svelte/store';

// Define the initial config value
const initialConfig = {
	// Add your initial configuration properties here
};

// Create a writable store with the initial config value
export const configStore = writable(initialConfig);
