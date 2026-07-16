<script lang="ts">
	import * as Empty from '$lib/components/ui/empty/index.js';
	import * as Table from '$lib/components/ui/table/index.js';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import type { Document } from '$lib/types/documents';
	import { formatCreatedAt } from '../format';
	import { toDisplay } from '../status';
	import StatusBadge from './status-badge.svelte';

	let { documents }: { documents: Document[] } = $props();
</script>

{#if documents.length === 0}
	<Empty.Root class="border-border rounded-lg border border-dashed">
		<Empty.Header>
			<Empty.Media variant="icon">
				<FileTextIcon />
			</Empty.Media>
			<Empty.Title>No documents yet</Empty.Title>
			<Empty.Description>
				Upload a PDF, TXT, or MD file and your bot will start answering from it.
			</Empty.Description>
		</Empty.Header>
	</Empty.Root>
{:else}
	<div class="rounded-lg border">
		<Table.Root>
			<Table.Header>
				<Table.Row>
					<Table.Head>Name</Table.Head>
					<Table.Head class="w-[160px]">Status</Table.Head>
					<Table.Head class="hidden w-[200px] md:table-cell">Added</Table.Head>
				</Table.Row>
			</Table.Header>
			<Table.Body>
				{#each documents as doc (doc.id)}
					<Table.Row>
						<Table.Cell class="font-medium">{doc.filename}</Table.Cell>
						<Table.Cell><StatusBadge status={doc.status} /></Table.Cell>
						<Table.Cell class="text-muted-foreground hidden md:table-cell">
							{formatCreatedAt(doc.createdAt)}
						</Table.Cell>
					</Table.Row>
					{#if doc.status === 'failed' || doc.status === 'quarantined' || doc.status === 'expired'}
						<Table.Row class="hover:bg-transparent">
							<Table.Cell colspan={3} class="text-muted-foreground pt-0 text-sm">
								{toDisplay(doc.status).description}
							</Table.Cell>
						</Table.Row>
					{/if}
				{/each}
			</Table.Body>
		</Table.Root>
	</div>
{/if}
