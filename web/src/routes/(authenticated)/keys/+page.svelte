<script lang="ts">
	import * as Alert from '$lib/components/ui/alert/index.js';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import KeyTable from '$lib/features/keys/components/key-table.svelte';
	import MintForm from '$lib/features/keys/components/mint-form.svelte';
	import RevealPanel from '$lib/features/keys/components/reveal-panel.svelte';
	import type { ActionData, PageData } from './$types';

	let { data, form }: { data: PageData; form: ActionData } = $props();

	// Present only in the response to a successful mint. A reload loses it, which is correct: the API
	// stores a hash and cannot show the key twice (invariant 22).
	const minted = $derived(form && 'minted' in form ? form.minted : null);
	const actionError = $derived(
		form && 'updateError' in form
			? form.updateError
			: form && 'revokeError' in form
				? form.revokeError
				: null
	);
</script>

<svelte:head><title>API keys</title></svelte:head>

<div class="flex flex-col gap-4">
	{#if minted}
		<RevealPanel apiKey={minted.apiKey} kind={minted.kind} widgetApiBase={data.widgetApiBase} />
	{/if}

	<MintForm data={data.form} />

	{#if actionError}
		<Alert.Root variant="destructive">
			<CircleAlertIcon />
			<Alert.Description>{actionError}</Alert.Description>
		</Alert.Root>
	{/if}

	{#if data.loadError}
		<!-- Not an empty list: telling a tenant they have no keys because our API blinked would invite
		     them to mint duplicates of keys they already have. -->
		<Alert.Root variant="destructive">
			<CircleAlertIcon />
			<Alert.Title>We couldn't load your keys</Alert.Title>
			<Alert.Description>
				This is a problem on our side. Try reloading in a moment — and don't create a new key yet,
				you may already have one.
			</Alert.Description>
		</Alert.Root>
	{:else}
		<KeyTable keys={data.keys} />
	{/if}
</div>
