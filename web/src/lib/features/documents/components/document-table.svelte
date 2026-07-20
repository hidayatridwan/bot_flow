<script lang="ts">
	import { enhance } from '$app/forms';
	import * as AlertDialog from '$lib/components/ui/alert-dialog/index.js';
	import * as Empty from '$lib/components/ui/empty/index.js';
	import * as Table from '$lib/components/ui/table/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
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
					<Table.Head class="w-[60px] text-right">Actions</Table.Head>
				</Table.Row>
			</Table.Header>
			<Table.Body>
				{#each documents as doc (doc.id)}
					<Table.Row>
						<Table.Cell class="font-medium">{doc.filename}</Table.Cell>
						<Table.Cell
							><StatusBadge status={doc.status} failureReason={doc.failureReason} /></Table.Cell
						>
						<Table.Cell class="text-muted-foreground hidden md:table-cell">
							{formatCreatedAt(doc.createdAt)}
						</Table.Cell>
						<Table.Cell class="text-right">
							<AlertDialog.Root>
								<AlertDialog.Trigger>
									{#snippet child({ props })}
										<Button
											{...props}
											variant="ghost"
											size="icon"
											class="text-muted-foreground hover:text-destructive"
											aria-label="Delete {doc.filename}"
										>
											<Trash2Icon />
										</Button>
									{/snippet}
								</AlertDialog.Trigger>
								<AlertDialog.Content>
									<AlertDialog.Header>
										<AlertDialog.Title>Delete this document?</AlertDialog.Title>
										<AlertDialog.Description>
											<strong>{doc.filename}</strong> and everything your bot learned from it are removed
											permanently. Your bot will stop answering from it. This cannot be undone.
										</AlertDialog.Description>
									</AlertDialog.Header>
									<AlertDialog.Footer>
										<AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
										<!-- Default `use:enhance`: on success it invalidates, so the poll's load reruns and
										     the row (now excluded — it is `deleting` or gone) drops out. -->
										<form method="POST" action="?/delete" use:enhance>
											<input type="hidden" name="id" value={doc.id} />
											<AlertDialog.Action type="submit">Delete</AlertDialog.Action>
										</form>
									</AlertDialog.Footer>
								</AlertDialog.Content>
							</AlertDialog.Root>
						</Table.Cell>
					</Table.Row>
					{#if doc.status === 'failed' || doc.status === 'quarantined' || doc.status === 'expired'}
						<Table.Row class="hover:bg-transparent">
							<Table.Cell colspan={4} class="text-muted-foreground pt-0 text-sm">
								{toDisplay(doc.status, doc.failureReason).description}
							</Table.Cell>
						</Table.Row>
					{/if}
				{/each}
			</Table.Body>
		</Table.Root>
	</div>
{/if}
