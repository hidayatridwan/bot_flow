<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { Button } from '$lib/components/ui/button/index.js';
	import BotMessageSquareIcon from '@lucide/svelte/icons/bot-message-square';

	/**
	 * The root error page. Without one, a 404, a 500 or any thrown `error()` rendered SvelteKit's
	 * built-in fallback: a status code on white, no styling, no navigation, no way back.
	 *
	 * **The message shown for a 5xx is ours, never the thrown one.** `page.error.message` on a server
	 * error can carry whatever an unhandled exception put there — a database URL, an upstream body, a
	 * stack frame — which is invariant 16's rule arriving at the last surface that can break it. A
	 * 404's message is safe because SvelteKit generates it, but there is nothing useful in "Not
	 * Found" that this page does not already say, so it is not shown either.
	 */
	const status = $derived(page.status);
	const notFound = $derived(status === 404);

	const heading = $derived(notFound ? 'Page not found' : 'Something went wrong');
	const body = $derived(
		notFound
			? "That link doesn't lead anywhere. It may be mistyped, or the page may have moved."
			: "This is a problem on our side, not with anything you did. Try again in a moment — if it keeps happening, it's ours to fix."
	);
</script>

<svelte:head><title>{heading}</title></svelte:head>

<div class="bg-muted flex min-h-svh flex-col items-center justify-center gap-6 p-6">
	<a href={resolve('/')} class="flex items-center gap-2 self-center font-medium">
		<span
			class="bg-primary text-primary-foreground flex size-6 items-center justify-center rounded-md"
		>
			<BotMessageSquareIcon class="size-4" />
		</span>
		BotFlow
	</a>

	<div class="flex max-w-md flex-col items-center gap-3 text-center">
		<p class="text-muted-foreground text-sm font-medium">{status}</p>
		<h1 class="text-2xl font-semibold tracking-tight">{heading}</h1>
		<p class="text-muted-foreground text-pretty">{body}</p>
	</div>

	<div class="flex flex-wrap items-center justify-center gap-2">
		<!-- Two destinations because there are two kinds of visitor here and this page cannot tell
		     them apart: `+error.svelte` renders when a layout's own load has failed, so it may have no
		     `data.user` to check. A signed-in tenant wants the dashboard; a stranger wants the front
		     page. Offering both beats guessing, and neither link can 404. -->
		<Button href={resolve('/dashboard')}>Go to dashboard</Button>
		<Button href={resolve('/')} variant="outline">Back to home</Button>
	</div>
</div>
