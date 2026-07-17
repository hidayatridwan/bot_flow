<script lang="ts">
	import CheckIcon from '@lucide/svelte/icons/check';
	import CopyIcon from '@lucide/svelte/icons/copy';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import * as Alert from '$lib/components/ui/alert/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import * as Card from '$lib/components/ui/card/index.js';
	import { Checkbox } from '$lib/components/ui/checkbox/index.js';
	import { FieldDescription } from '$lib/components/ui/field/index.js';
	import * as InputGroup from '$lib/components/ui/input-group/index.js';
	import { Label } from '$lib/components/ui/label/index.js';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();

	let copied = $state(false);
	let acknowledged = $state(false);

	async function copy() {
		await navigator.clipboard.writeText(data.apiKey);
		copied = true;
		setTimeout(() => (copied = false), 2000);
	}
</script>

<div class="mx-auto flex w-full max-w-xl flex-col gap-6 py-10">
	<Card.Root>
		<Card.Header>
			<Card.Title class="text-xl">Your secret API key</Card.Title>
			<Card.Description>
				This is the key your server uses to upload documents and ask questions on your behalf.
			</Card.Description>
		</Card.Header>
		<Card.Content class="flex flex-col gap-6">
			<Alert.Root variant="destructive">
				<TriangleAlertIcon />
				<Alert.Title>Copy this key now — it will not be shown again</Alert.Title>
				<Alert.Description>
					We only store a hash of it, so we cannot show it to you a second time. If you lose it,
					leave this page, or refresh, you can create a new one under
					<a href="/keys" class="underline underline-offset-4">API keys</a>.
				</Alert.Description>
			</Alert.Root>

			<div class="flex flex-col gap-2">
				<Label for="api-key">Secret key</Label>
				<InputGroup.Root>
					<InputGroup.Input id="api-key" readonly value={data.apiKey} />
					<InputGroup.Addon align="inline-end">
						<InputGroup.Button size="icon-xs" onclick={copy} aria-label="Copy API key">
							{#if copied}
								<CheckIcon />
							{:else}
								<CopyIcon />
							{/if}
						</InputGroup.Button>
					</InputGroup.Addon>
				</InputGroup.Root>
				<FieldDescription>
					Keep it on your server. Never put a secret key in a web page or a mobile app.
				</FieldDescription>
			</div>

			<div class="flex items-center gap-3">
				<Checkbox id="acknowledge" bind:checked={acknowledged} />
				<Label for="acknowledge" class="font-normal">I have saved my API key somewhere safe</Label>
			</div>
		</Card.Content>
		<Card.Footer>
			<Button href="/dashboard" disabled={!acknowledged} class="w-full"
				>Continue to dashboard</Button
			>
		</Card.Footer>
	</Card.Root>
</div>
