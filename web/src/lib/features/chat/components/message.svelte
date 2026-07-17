<script lang="ts">
	import { cn } from '$lib/utils';
	import Spinner from '$lib/components/ui/spinner/spinner.svelte';

	/**
	 * One turn in the transcript.
	 *
	 * **Plain text, no markdown renderer**, for two reasons that both matter. The widget renders with
	 * `textContent`, so a renderer here would quietly break this page's only claim — that it shows what
	 * an end user sees. And it would be an HTML injection surface fed by an LLM whose output is
	 * assembled from tenant documents; `white-space: pre-wrap` keeps the model's own line breaks
	 * without ever interpreting its bytes.
	 */
	let {
		role,
		text,
		pending = false
	}: { role: 'user' | 'assistant'; text: string; pending?: boolean } = $props();
</script>

<div class={cn('flex', role === 'user' ? 'justify-end' : 'justify-start')}>
	<div
		class={cn(
			'max-w-[85%] rounded-lg px-3 py-2 text-sm whitespace-pre-wrap',
			role === 'user' ? 'bg-primary text-primary-foreground' : 'bg-muted'
		)}
	>
		{#if pending && !text}
			<!-- The gap between "sent" and the first token is two model calls long (the rewrite, then
			     retrieval), so it is long enough to read as a broken page without this. -->
			<span class="text-muted-foreground flex items-center gap-2">
				<Spinner class="size-3" />
				Thinking…
			</span>
		{:else}
			{text}
		{/if}
	</div>
</div>
