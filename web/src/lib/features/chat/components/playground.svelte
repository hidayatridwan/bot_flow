<script lang="ts">
	import { resolve } from '$app/paths';
	import * as Alert from '$lib/components/ui/alert/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Textarea } from '$lib/components/ui/textarea/index.js';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import SendIcon from '@lucide/svelte/icons/send';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import { ask, type Source } from '../ask';
	import { toDisplays, type SourceDisplay } from '../sources';
	import Message from './message.svelte';
	import SourcesList from './sources-list.svelte';

	let {
		filenames,
		readyCount,
		loadError
	}: { filenames: Record<string, string>; readyCount: number; loadError: boolean } = $props();

	interface Turn {
		question: string;
		answer: string;
		sources: SourceDisplay[];
		refused: boolean;
		error: string | null;
		pending: boolean;
	}

	let turns = $state<Turn[]>([]);
	let draft = $state('');
	let busy = $state(false);
	// Held in component state, not the URL: it is a handle to server-side history, not a location.
	let conversationId = $state('');
	let log = $state<HTMLDivElement | null>(null);

	async function send() {
		const query = draft.trim();
		if (!query || busy) return;

		draft = '';
		busy = true;
		const turn: Turn = {
			question: query,
			answer: '',
			sources: [],
			refused: false,
			error: null,
			pending: true
		};
		turns = [...turns, turn];
		const i = turns.length - 1;
		scrollDown();

		const outcome = await ask(
			{ query, conversationId },
			{
				onConversation: (id) => (conversationId = id),
				onSources: (s: Source[]) => {
					// Citations land *before* the first token — the API knows them the moment retrieval
					// returns. Rendering them as they arrive is a real property of the product, and only
					// the streaming route has it.
					turns[i].sources = toDisplays(s, filenames);
					scrollDown();
				},
				onToken: (text) => {
					turns[i].answer += text;
					scrollDown();
				}
			},
			{ fetch: window.fetch.bind(window) }
		);

		turns[i].pending = false;
		if (outcome.ok) {
			turns[i].refused = outcome.refused;
		} else {
			// Whatever text arrived stays on screen — it is true, and only the rest is missing.
			turns[i].error = outcome.message;
		}
		busy = false;
		scrollDown();
	}

	function newChat() {
		// Dropping the transcript must drop the id with it. Otherwise the server keeps resolving
		// follow-ups against history the user can no longer see — widget.js learned this already.
		turns = [];
		conversationId = '';
		draft = '';
	}

	function scrollDown() {
		requestAnimationFrame(() => log?.scrollTo({ top: log.scrollHeight }));
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) {
			e.preventDefault();
			send();
		}
	}
</script>

<div class="flex h-full flex-col gap-4">
	{#if loadError}
		<Alert.Root variant="destructive">
			<CircleAlertIcon />
			<Alert.Description>
				We couldn't load your documents, so citations below may not show which file they came from.
				Answers are unaffected.
			</Alert.Description>
		</Alert.Root>
	{:else if readyCount === 0}
		<!-- Without this, a tenant's first experience is a refusal that reads as a bug. Invariant 4
		     guarantees every question is declined when nothing is indexed. -->
		<Alert.Root>
			<CircleAlertIcon />
			<Alert.Title>No documents are ready yet</Alert.Title>
			<Alert.Description>
				The bot answers only from your documents, so it will decline every question until one
				finishes indexing.
				<a href={resolve('/documents')} class="underline">Upload a document</a> first.
			</Alert.Description>
		</Alert.Root>
	{/if}

	<div
		bind:this={log}
		class="bg-muted/20 min-h-88 flex-1 space-y-4 overflow-y-auto rounded-lg border p-4"
	>
		{#if !turns.length}
			<p class="text-muted-foreground py-12 text-center text-sm">
				Ask something your documents answer, and you'll see exactly what your visitors would.
			</p>
		{/if}

		{#each turns as turn, i (i)}
			<div class="space-y-2">
				<Message role="user" text={turn.question} />
				<Message role="assistant" text={turn.answer} pending={turn.pending} />
				{#if !turn.pending}
					<SourcesList sources={turn.sources} refused={turn.refused} />
				{/if}
				{#if turn.error}
					<!-- Generic by construction: `ask.ts` never passes the API's own words through
					     (invariant 16). -->
					<p class="text-destructive pl-1 text-xs">{turn.error}</p>
				{/if}
			</div>
		{/each}
	</div>

	<noscript>
		<Alert.Root>
			<CircleAlertIcon />
			<Alert.Title>The playground needs JavaScript</Alert.Title>
			<Alert.Description>
				It streams the answer token by token, which is the point — it is what your visitors see.
			</Alert.Description>
		</Alert.Root>
	</noscript>

	<div class="flex gap-2">
		<Textarea
			bind:value={draft}
			onkeydown={onKeydown}
			placeholder="Ask a question…"
			rows={2}
			disabled={busy}
			class="resize-none"
			aria-label="Your question"
		/>
		<div class="flex flex-col gap-2">
			<Button onclick={send} disabled={busy || !draft.trim()} aria-label="Send">
				<SendIcon />
			</Button>
			<Button
				variant="outline"
				onclick={newChat}
				disabled={busy || !turns.length}
				aria-label="New chat"
			>
				<PlusIcon />
			</Button>
		</div>
	</div>
</div>
