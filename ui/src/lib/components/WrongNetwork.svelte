<script lang="ts">
	import { Alert, Button } from 'flowbite-svelte'
	import { switchNetwork } from '$lib/ethereum'
	import { bridge } from '$lib/config'
</script>

<Alert color="red">Wrong network: please switch to {bridge.networkName}</Alert>
<Button class="w-full" color="red" on:click={switchNetwork}>Switch to {bridge.networkName}</Button>
