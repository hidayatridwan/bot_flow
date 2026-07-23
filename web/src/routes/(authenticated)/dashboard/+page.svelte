<script lang="ts">
	import * as Alert from '$lib/components/ui/alert/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import * as Card from '$lib/components/ui/card/index.js';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import CircleCheckIcon from '@lucide/svelte/icons/circle-check';
	import CircleDashedIcon from '@lucide/svelte/icons/circle-dashed';
	import CircleIcon from '@lucide/svelte/icons/circle';
	import { formatCount, readinessSteps } from '$lib/features/dashboard/readiness';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();

	const steps = $derived(readinessSteps(data.counts, data.publishableKeys));
	const live = $derived(data.counts.ready > 0 && data.publishableKeys > 0);
</script>

<svelte:head><title>Dashboard</title></svelte:head>

<div class="flex flex-col gap-4">
	{#if data.loadError}
		<!-- Not zeroes: telling a tenant their bot has no documents because our API blinked would send
		     them to re-upload a library that is perfectly intact. -->
		<Alert.Root variant="destructive">
			<CircleAlertIcon />
			<Alert.Title>We couldn't load your workspace</Alert.Title>
			<Alert.Description>
				This is a problem on our side, not with your data. Try reloading in a moment.
			</Alert.Description>
		</Alert.Root>
	{:else}
		<Card.Root>
			<Card.Header>
				<Card.Title>{live ? 'Your bot is ready' : 'Get your bot answering'}</Card.Title>
				<Card.Description>
					{#if live}
						It answers from
						{formatCount(data.counts.ready, data.counts.partial)}
						indexed document{data.counts.ready === 1 && !data.counts.partial ? '' : 's'}, and only
						from those.
					{:else}
						Three steps. Your bot answers only from documents you upload — until one is indexed, it
						declines every question.
					{/if}
				</Card.Description>
			</Card.Header>
			<Card.Content class="flex flex-col gap-4">
				{#each steps as step (step.id)}
					<div class="flex items-start gap-3">
						<div class="mt-0.5 shrink-0">
							{#if step.state === 'done'}
								<CircleCheckIcon class="size-5 text-emerald-600 dark:text-emerald-400" />
							{:else if step.state === 'pending'}
								<CircleDashedIcon class="text-muted-foreground size-5" />
							{:else}
								<CircleIcon class="text-muted-foreground/40 size-5" />
							{/if}
						</div>
						<div class="flex-1">
							<div class="text-sm font-medium">{step.title}</div>
							<p class="text-muted-foreground text-sm">{step.detail}</p>
						</div>
						<Button
							href={step.href}
							variant={step.state === 'pending' ? 'default' : 'outline'}
							size="sm"
						>
							{step.cta}
						</Button>
					</div>
				{/each}
			</Card.Content>
		</Card.Root>

		{#if data.counts.processing > 0 || data.counts.failed > 0}
			<div class="grid gap-4 sm:grid-cols-2">
				{#if data.counts.processing > 0}
					<Card.Root>
						<Card.Header>
							<Card.Description>Still indexing</Card.Description>
							<Card.Title class="text-2xl">{data.counts.processing}</Card.Title>
						</Card.Header>
						<Card.Content>
							<p class="text-muted-foreground text-sm">
								Not answerable yet. Nothing for you to do — the documents page updates itself.
							</p>
						</Card.Content>
					</Card.Root>
				{/if}
				{#if data.counts.failed > 0}
					<Card.Root>
						<Card.Header>
							<Card.Description>Needs attention</Card.Description>
							<Card.Title class="text-2xl">{data.counts.failed}</Card.Title>
						</Card.Header>
						<Card.Content class="flex items-center justify-between gap-2">
							<!-- Deliberately not naming a cause here. Whether a failure is the tenant's to fix
							     or ours is decided per-document by `failure_reason`, and the documents page is
							     where that copy lives — one place, not two that can disagree. -->
							<p class="text-muted-foreground text-sm">
								Some documents didn't finish. The library says why.
							</p>
							<Button href="/documents" variant="outline" size="sm">Review</Button>
						</Card.Content>
					</Card.Root>
				{/if}
			</div>
		{/if}
	{/if}
</div>
