<script lang="ts">
	import '../layout.css';
	import { resolve } from '$app/paths';
	import favicon from '$lib/assets/favicon.svg';
	import BotMessageSquareIcon from '@lucide/svelte/icons/bot-message-square';

	let { children } = $props();
</script>

<svelte:head><link rel="icon" href={favicon} /></svelte:head>
<div class="bg-muted flex min-h-svh flex-col items-center justify-center gap-6 p-6 md:p-10">
	<div class="flex w-full max-w-sm flex-col gap-6">
		<a href={resolve('/')} class="flex items-center gap-2 self-center font-medium">
			<div
				class="bg-primary text-primary-foreground flex size-6 items-center justify-center rounded-md"
			>
				<BotMessageSquareIcon class="size-4" />
			</div>
			BotFlow
		</a>
		{@render children()}
	</div>
</div>
