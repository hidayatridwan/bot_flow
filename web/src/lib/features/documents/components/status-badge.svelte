<script lang="ts">
	import { Badge } from '$lib/components/ui/badge/index.js';
	import { Spinner } from '$lib/components/ui/spinner/index.js';
	import * as Tooltip from '$lib/components/ui/tooltip/index.js';
	import type { DocumentStatus } from '$lib/types/documents';
	import { toDisplay } from '../status';

	let { status }: { status: DocumentStatus | 'unknown' } = $props();

	const display = $derived(toDisplay(status));

	// The registry's Badge has no `success` variant (default | secondary | destructive | outline |
	// ghost | link), so a settled-and-happy state needs a colour at the call site. Passing `class` to
	// an installed component is composition, not a new primitive — the ui/ tree stays regenerable.
	const readyClass =
		'bg-emerald-500/10 text-emerald-700 dark:text-emerald-400 dark:bg-emerald-500/20';
</script>

<Tooltip.Provider>
	<Tooltip.Root>
		<Tooltip.Trigger>
			<Badge variant={display.variant} class={status === 'ready' ? readyClass : undefined}>
				{#if display.spinner}
					<Spinner class="size-3" />
				{/if}
				{display.label}
			</Badge>
		</Tooltip.Trigger>
		<Tooltip.Content>
			<p class="max-w-xs">{display.description}</p>
		</Tooltip.Content>
	</Tooltip.Root>
</Tooltip.Provider>
