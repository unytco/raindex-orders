<script lang="ts">
	import { Alert } from 'flowbite-svelte'
	import { PAUSED_CONTACT, PAUSED_LEAD } from '$lib/pause'
</script>

<Alert color="yellow">
	{PAUSED_LEAD}
	<a href="mailto:{PAUSED_CONTACT}" class="font-medium underline">{PAUSED_CONTACT}</a>.
</Alert>
