<script lang="ts">
	import * as Alert from '$lib/components/ui/alert/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import * as Card from '$lib/components/ui/card/index.js';
	import { Spinner } from '$lib/components/ui/spinner/index.js';
	import UploadIcon from '@lucide/svelte/icons/upload';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import { ALLOWED_EXTENSIONS } from '../schema';
	import { uploadFile } from '../upload';

	let { onuploaded }: { onuploaded: () => void } = $props();

	let input = $state<HTMLInputElement | null>(null);
	let busy = $state(false);
	let message = $state<string | null>(null);

	const accept = ALLOWED_EXTENSIONS.map((e) => `.${e}`).join(',');

	async function onchange(event: Event) {
		const picked = (event.currentTarget as HTMLInputElement).files?.[0];
		if (!picked) return;

		busy = true;
		message = null;

		const result = await uploadFile(picked, { fetch });

		busy = false;
		// Always clear, or picking the same file twice fires no `change` event and the second attempt
		// silently does nothing.
		if (input) input.value = '';

		if (result.ok) {
			onuploaded();
		} else {
			message = result.message;
		}
	}
</script>

<Card.Root>
	<Card.Header>
		<Card.Title>Upload a document</Card.Title>
		<Card.Description>
			PDF, TXT, or MD, up to 25 MB. Your bot answers only from what you upload here.
		</Card.Description>
	</Card.Header>
	<Card.Content class="flex flex-col gap-4">
		<!--
			The bytes go from this browser straight to storage, so this is the one page in the app with
			no form action — there is nothing to progressively enhance towards. A multipart <form> would
			proxy the file through Node, which is exactly the deprecated POST /documents path, rebuilt
			one layer up. Reading the list below still works without JS; only this card needs it.
		-->
		<input
			bind:this={input}
			type="file"
			{accept}
			{onchange}
			disabled={busy}
			class="hidden"
			aria-hidden="true"
			tabindex="-1"
		/>

		<div>
			<Button onclick={() => input?.click()} disabled={busy}>
				{#if busy}
					<Spinner />
					Uploading…
				{:else}
					<UploadIcon />
					Choose a file
				{/if}
			</Button>
		</div>

		<noscript>
			<Alert.Root>
				<CircleAlertIcon />
				<Alert.Title>Uploading needs JavaScript</Alert.Title>
				<Alert.Description>Your documents are listed below either way.</Alert.Description>
			</Alert.Root>
		</noscript>

		{#if message}
			<Alert.Root variant="destructive">
				<CircleAlertIcon />
				<Alert.Description>{message}</Alert.Description>
			</Alert.Root>
		{/if}
	</Card.Content>
</Card.Root>
