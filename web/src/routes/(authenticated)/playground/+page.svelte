<script lang="ts">
	import { resolve } from '$app/paths';
	import * as Card from '$lib/components/ui/card/index.js';
	import Playground from '$lib/features/chat/components/playground.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
</script>

<svelte:head>
	<title>Playground · BotFlow</title>
</svelte:head>

<div class="flex flex-1 flex-col gap-4 p-4 pt-0">
	<Card.Root class="flex flex-1 flex-col">
		<Card.Header>
			<Card.Title>Playground</Card.Title>
			<Card.Description>
				Ask your bot a question and see the answer your visitors would get, streamed the same way,
				with the passages it was grounded in.
			</Card.Description>
		</Card.Header>
		<Card.Content class="flex-1">
			<Playground
				filenames={data.filenames}
				readyCount={data.readyCount}
				loadError={data.loadError}
			/>
		</Card.Content>
		<Card.Footer>
			<!--
				The one thing this page cannot show, said plainly.

				It authenticates with your dashboard session; the live widget authenticates with a `pk_`
				bound to an Origin. So it cannot reproduce an allowed_origins mismatch — the most likely
				go-live failure, and the one that 403s with nothing in any log to explain it. A preview
				that hides the most common production failure is worse than no preview if it does not say
				so.
			-->
			<p class="text-muted-foreground text-xs">
				This uses your dashboard session. Your live widget uses a publishable key locked to an
				origin, so a working playground doesn't prove your key's
				<a href={resolve('/keys')} class="underline">allowed origins</a> are right.
			</p>
		</Card.Footer>
	</Card.Root>
</div>
