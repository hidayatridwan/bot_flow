<script lang="ts">
	import { Button } from '$lib/components/ui/button/index.js';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import ChevronsLeftIcon from '@lucide/svelte/icons/chevrons-left';

	/**
	 * Page navigation for the document list.
	 *
	 * **Anchors, not buttons, and that is a decision rather than a shortcut.** Reading the library is
	 * the half of this page that works without JavaScript (invariant 24 spends that guarantee on
	 * *uploading* only), so paging has to be a link the browser can follow on its own. It also means
	 * the Back button walks back through pages for free — a keyset cursor has no natural "previous",
	 * and history is the honest way to provide one rather than keeping a cursor stack in memory that
	 * a reload would lose.
	 *
	 * Nothing renders when there is a single page, which is every new tenant.
	 */
	let { nextCursor, isFirstPage }: { nextCursor: string | null; isFirstPage: boolean } = $props();

	// `encodeURIComponent`, because the cursor carries `+` (the UTC offset). A bare `+` in a query
	// string decodes to a space, which corrupts the timestamp rather than merely looking wrong — and
	// the damage shows up only at a page boundary.
	const nextHref = $derived(nextCursor ? `?before=${encodeURIComponent(nextCursor)}` : null);
</script>

{#if nextHref || !isFirstPage}
	<nav class="flex items-center justify-between gap-2" aria-label="Document pages">
		<div>
			{#if !isFirstPage}
				<Button href="/documents" variant="ghost" size="sm" data-sveltekit-noscroll>
					<ChevronsLeftIcon />
					Newest
				</Button>
			{/if}
		</div>
		<div>
			{#if nextHref}
				<!-- "Older", not "Next": the ordering is newest-first, and a tenant reasons about their
				     library in time rather than in page numbers — which a keyset cursor cannot offer
				     anyway. Promising "Page 2" would be promising something we do not have. -->
				<Button href={nextHref} variant="outline" size="sm" data-sveltekit-noscroll>
					Older
					<ChevronRightIcon />
				</Button>
			{/if}
		</div>
	</nav>
{/if}
