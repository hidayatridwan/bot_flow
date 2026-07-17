<script lang="ts">
	import { enhance } from '$app/forms';
	import * as AlertDialog from '$lib/components/ui/alert-dialog/index.js';
	import * as Empty from '$lib/components/ui/empty/index.js';
	import * as Table from '$lib/components/ui/table/index.js';
	import { Badge } from '$lib/components/ui/badge/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Textarea } from '$lib/components/ui/textarea/index.js';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import { formatCreatedAt } from '$lib/features/documents/format';
	import type { ApiKeyDto } from '$lib/server/api/keys';

	let { keys }: { keys: ApiKeyDto[] } = $props();

	// Which row's origin editor is open, keyed by hash. Only publishable keys are editable.
	let editing = $state<string | null>(null);

	// The hash is not secret — it is the revoke/patch handle — but it is 64 hex chars and useless to
	// read in full.
	const short = (hash: string) => `${hash.slice(0, 12)}…`;
</script>

{#if keys.length === 0}
	<Empty.Root class="border-border rounded-lg border border-dashed">
		<Empty.Header>
			<Empty.Media variant="icon"><KeyRoundIcon /></Empty.Media>
			<Empty.Title>No keys yet</Empty.Title>
			<Empty.Description>
				Create a publishable key to put the chat widget on your site.
			</Empty.Description>
		</Empty.Header>
	</Empty.Root>
{:else}
	<div class="rounded-lg border">
		<Table.Root>
			<Table.Header>
				<Table.Row>
					<Table.Head>Label</Table.Head>
					<Table.Head class="w-[130px]">Type</Table.Head>
					<Table.Head>Allowed origins</Table.Head>
					<Table.Head class="hidden w-[190px] md:table-cell">Created</Table.Head>
					<Table.Head class="w-[100px] text-right">Actions</Table.Head>
				</Table.Row>
			</Table.Header>
			<Table.Body>
				{#each keys as key (key.key_hash)}
					<Table.Row>
						<Table.Cell class="font-medium">
							{key.label || 'default'}
							<div class="text-muted-foreground font-mono text-xs">{short(key.key_hash)}</div>
						</Table.Cell>
						<Table.Cell>
							<Badge variant={key.kind === 'secret' ? 'secondary' : 'outline'}>
								{key.kind === 'secret' ? 'Secret' : 'Publishable'}
							</Badge>
						</Table.Cell>
						<Table.Cell class="text-sm">
							{#if key.kind === 'secret'}
								<span class="text-muted-foreground">Not origin-checked</span>
							{:else if key.allowed_origins.length === 0}
								<!-- The API now refuses to mint this, but rows predating that rule can exist. -->
								<span class="text-destructive">None — this key cannot answer anywhere</span>
							{:else}
								<ul class="font-mono text-xs">
									{#each key.allowed_origins as origin (origin)}
										<li>{origin}</li>
									{/each}
								</ul>
							{/if}
						</Table.Cell>
						<Table.Cell class="text-muted-foreground hidden md:table-cell">
							{formatCreatedAt(key.created_at)}
						</Table.Cell>
						<Table.Cell class="text-right">
							<div class="flex justify-end gap-1">
								{#if key.kind === 'publishable'}
									<Button
										variant="ghost"
										size="icon"
										aria-label="Edit origins"
										onclick={() => (editing = editing === key.key_hash ? null : key.key_hash)}
									>
										<PencilIcon />
									</Button>
								{/if}

								<AlertDialog.Root>
									<AlertDialog.Trigger>
										{#snippet child({ props })}
											<Button {...props} variant="ghost" size="sm" class="text-destructive">
												Revoke
											</Button>
										{/snippet}
									</AlertDialog.Trigger>
									<AlertDialog.Content>
										<AlertDialog.Header>
											<AlertDialog.Title>Revoke this key?</AlertDialog.Title>
											<AlertDialog.Description>
												Anything using <strong>{key.label || 'default'}</strong> stops working immediately.
												This cannot be undone — you would have to create a new key and update wherever
												it is used.
											</AlertDialog.Description>
										</AlertDialog.Header>
										<AlertDialog.Footer>
											<AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
											<form method="POST" action="?/revoke" use:enhance>
												<input type="hidden" name="keyHash" value={key.key_hash} />
												<AlertDialog.Action type="submit">Revoke key</AlertDialog.Action>
											</form>
										</AlertDialog.Footer>
									</AlertDialog.Content>
								</AlertDialog.Root>
							</div>
						</Table.Cell>
					</Table.Row>

					{#if editing === key.key_hash}
						<Table.Row class="hover:bg-transparent">
							<Table.Cell colspan={5} class="pt-0">
								<!-- Editing beats re-minting: a pk_ is public and expected to be stolen, so
								     rotating it to add a domain buys nothing. The allow-list is the containment. -->
								<form
									method="POST"
									action="?/updateOrigins"
									use:enhance={() =>
										async ({ update }) => {
											await update();
											editing = null;
										}}
									class="flex flex-col gap-2"
								>
									<input type="hidden" name="keyHash" value={key.key_hash} />
									<Textarea name="origins" rows={3} value={key.allowed_origins.join('\n')} />
									<div class="flex gap-2">
										<Button type="submit" size="sm">Save origins</Button>
										<Button variant="ghost" size="sm" onclick={() => (editing = null)}>
											Cancel
										</Button>
									</div>
								</form>
							</Table.Cell>
						</Table.Row>
					{/if}
				{/each}
			</Table.Body>
		</Table.Root>
	</div>
{/if}
