<script lang="ts">
	import CheckIcon from '@lucide/svelte/icons/check';
	import CopyIcon from '@lucide/svelte/icons/copy';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import * as Alert from '$lib/components/ui/alert/index.js';
	import * as Card from '$lib/components/ui/card/index.js';
	import * as InputGroup from '$lib/components/ui/input-group/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Label } from '$lib/components/ui/label/index.js';
	import { FieldDescription } from '$lib/components/ui/field/index.js';
	import { embedSnippet } from '../embed';

	let { apiKey, kind, widgetApiBase }: { apiKey: string; kind: string; widgetApiBase: string } =
		$props();

	// Mint time is the only moment the raw key exists, so it is the only moment a complete snippet can
	// be built. A secret key gets none: `embedSnippet` throws on an sk_ rather than print it into
	// something designed to be pasted in public.
	const snippet = $derived(
		kind === 'publishable' ? embedSnippet({ apiBase: widgetApiBase, publicKey: apiKey }) : null
	);

	let copiedKey = $state(false);
	let copiedSnippet = $state(false);

	async function copy(text: string, which: 'key' | 'snippet') {
		await navigator.clipboard.writeText(text);
		if (which === 'key') {
			copiedKey = true;
			setTimeout(() => (copiedKey = false), 2000);
		} else {
			copiedSnippet = true;
			setTimeout(() => (copiedSnippet = false), 2000);
		}
	}
</script>

<Card.Root>
	<Card.Header>
		<Card.Title>Your new {kind} key</Card.Title>
		<Card.Description>
			{#if kind === 'publishable'}
				Paste the snippet below into your site. This key is safe to publish — it can only ask
				questions, and only from the origins you allow-listed.
			{:else}
				Keep this on your server. A secret key can do everything your account can.
			{/if}
		</Card.Description>
	</Card.Header>
	<Card.Content class="flex flex-col gap-6">
		<Alert.Root variant="destructive">
			<TriangleAlertIcon />
			<Alert.Title>Copy this now — it will not be shown again</Alert.Title>
			<Alert.Description>
				We store only a hash, so we cannot show it a second time. Leaving or reloading this page
				loses it, and you would need to mint another.
			</Alert.Description>
		</Alert.Root>

		<div class="flex flex-col gap-2">
			<Label for="new-key">{kind === 'publishable' ? 'Publishable' : 'Secret'} key</Label>
			<InputGroup.Root>
				<InputGroup.Input id="new-key" readonly value={apiKey} />
				<InputGroup.Addon align="inline-end">
					<InputGroup.Button
						size="icon-xs"
						onclick={() => copy(apiKey, 'key')}
						aria-label="Copy API key"
					>
						{#if copiedKey}<CheckIcon />{:else}<CopyIcon />{/if}
					</InputGroup.Button>
				</InputGroup.Addon>
			</InputGroup.Root>
			{#if kind === 'secret'}
				<FieldDescription>Never put a secret key in a web page or a mobile app.</FieldDescription>
			{/if}
		</div>

		{#if snippet}
			<div class="flex flex-col gap-2">
				<Label for="snippet">Embed snippet</Label>
				<pre
					id="snippet"
					class="bg-muted overflow-x-auto rounded-md p-3 text-xs leading-relaxed"><code
						>{snippet}</code
					></pre>
				<div class="flex items-center gap-2">
					<Button variant="outline" size="sm" onclick={() => copy(snippet, 'snippet')}>
						{#if copiedSnippet}<CheckIcon />{:else}<CopyIcon />{/if}
						Copy snippet
					</Button>
				</div>
				<FieldDescription>
					Download <code>widget.js</code> and host it on your site, then point the
					<code>src</code> at it.
				</FieldDescription>
			</div>
		{/if}
	</Card.Content>
</Card.Root>
