<script lang="ts">
	import * as Collapsible from '$lib/components/ui/collapsible/index.js';
	import { Badge } from '$lib/components/ui/badge/index.js';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import CircleHelpIcon from '@lucide/svelte/icons/circle-help';
	import type { SourceDisplay } from '../sources';

	/**
	 * The passages an answer was built from.
	 *
	 * This is the first surface in the product to render citations at all. The server has always
	 * emitted them and `widget.js` has always ignored them, which left invariant 5 — the model may not
	 * write `[n]` markers, *because* citations are structured data — with nothing to point at.
	 *
	 * They sit beside the answer rather than inside it. That is not a layout preference: markers in the
	 * prose are exactly what the model is forbidden to produce, so inlining them here would rebuild by
	 * hand the thing the system prompt spends a sentence preventing.
	 */
	let { sources, refused }: { sources: SourceDisplay[]; refused: boolean } = $props();
</script>

{#if refused}
	<!-- Invariant 4, rendered as what it is. Nothing cleared the relevance floor, so the API answered
	     from its canned line and never called the model. That is the system keeping its promise — a
	     destructive alert here would tell a tenant their bot is broken at the moment it is working. -->
	<p class="text-muted-foreground pl-1 text-xs">
		No passages matched, so the bot declined to answer rather than guess.
	</p>
{:else if sources.length}
	<div class="space-y-1 pl-1">
		<p class="text-muted-foreground text-xs font-medium">Grounded in</p>
		{#each sources as source (source.index)}
			<Collapsible.Root>
				<Collapsible.Trigger
					class="group hover:bg-muted/50 flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs"
				>
					<ChevronRightIcon
						class="text-muted-foreground size-3 shrink-0 transition-transform group-data-[state=open]:rotate-90"
					/>
					<Badge variant="outline" class="shrink-0 font-mono text-[10px]">{source.label}</Badge>
					{#if source.unattributed}
						<CircleHelpIcon class="text-muted-foreground size-3 shrink-0" />
					{:else}
						<FileTextIcon class="text-muted-foreground size-3 shrink-0" />
					{/if}
					<span
						class="truncate {source.unattributed ? 'text-muted-foreground italic' : ''}"
						title={source.name}
					>
						{source.name}
					</span>
					<span class="text-muted-foreground ml-auto shrink-0 font-mono text-[10px]">
						{source.score}
					</span>
				</Collapsible.Trigger>
				<Collapsible.Content>
					<p
						class="text-muted-foreground border-muted mt-1 ml-4 border-l-2 py-1 pl-3 text-xs whitespace-pre-wrap"
					>
						{source.text}
					</p>
					{#if source.unattributed}
						<!-- The /ingest debt, surfaced where a tenant actually meets it. The passage answered
						     the question; what it lacks is any record it belongs to, so it can never be
						     listed or removed. -->
						<p class="text-muted-foreground/70 mt-1 ml-4 pl-3 text-[10px]">
							Indexed directly rather than uploaded, so it belongs to no document.
						</p>
					{/if}
				</Collapsible.Content>
			</Collapsible.Root>
		{/each}
	</div>
{/if}
