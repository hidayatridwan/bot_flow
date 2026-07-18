<script lang="ts">
	import { invalidate } from '$app/navigation';
	import * as Alert from '$lib/components/ui/alert/index.js';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import DocumentTable from '$lib/features/documents/components/document-table.svelte';
	import UploadCard from '$lib/features/documents/components/upload-card.svelte';
	import { POLL_MIN_MS, nextInterval } from '$lib/features/documents/poll';
	import { isTransient } from '$lib/features/documents/status';
	import type { ActionData, PageData } from './$types';

	let { data, form }: { data: PageData; form: ActionData } = $props();

	const deleteError = $derived(form && 'deleteError' in form ? form.deleteError : null);

	const anyTransient = $derived(data.documents.some((d) => isTransient(d.status)));
	// The signature of "did anything move?" — cheaper and more precise than deep-comparing the rows.
	const fingerprint = $derived(data.documents.map((d) => `${d.id}:${d.status}`).join('|'));

	/**
	 * Poll only while something is actually in flight.
	 *
	 * There is no upload-completion callback by design (storage announces the upload itself), so the
	 * only way to see `uploading → processing → ready` is to ask again. Three gates keep that honest:
	 * a tenant whose documents are all settled starts no timer at all; a hidden tab stops asking; and
	 * the interval backs off while nothing changes, because an upload that never arrives sits for
	 * ~20 minutes before the reaper settles it.
	 *
	 * Each tick costs two API calls, not one: hooks.server.ts resolves the session via GET /auth/me
	 * before this load runs GET /documents. That is the number that would justify a dedicated
	 * endpoint if this ever shows up in a profile.
	 */
	$effect(() => {
		if (!anyTransient) return;

		let delay = POLL_MIN_MS;
		let timer: ReturnType<typeof setTimeout>;
		let stopped = false;
		let previous = fingerprint;

		const tick = async () => {
			if (stopped) return;
			if (document.visibilityState === 'visible') {
				await invalidate('documents:list');
				if (stopped) return;
				delay = nextInterval(delay, fingerprint !== previous);
				previous = fingerprint;
			}
			timer = setTimeout(tick, delay);
		};

		// Resume promptly when the tab comes back, rather than waiting out a backed-off timer.
		const onVisible = () => {
			if (document.visibilityState === 'visible') {
				delay = POLL_MIN_MS;
				clearTimeout(timer);
				timer = setTimeout(tick, delay);
			}
		};
		document.addEventListener('visibilitychange', onVisible);

		timer = setTimeout(tick, delay);

		return () => {
			stopped = true;
			clearTimeout(timer);
			document.removeEventListener('visibilitychange', onVisible);
		};
	});
</script>

<svelte:head><title>Documents</title></svelte:head>

<div class="flex flex-col gap-4">
	<UploadCard onuploaded={() => invalidate('documents:list')} />

	{#if deleteError}
		<Alert.Root variant="destructive">
			<CircleAlertIcon />
			<Alert.Description>{deleteError}</Alert.Description>
		</Alert.Root>
	{/if}

	{#if data.loadError}
		<!-- Not an empty table: telling a tenant their library is gone because our API blinked would
		     invite them to re-upload everything. An outage is not an empty library. -->
		<Alert.Root variant="destructive">
			<CircleAlertIcon />
			<Alert.Title>We couldn't load your documents</Alert.Title>
			<Alert.Description>
				This is a problem on our side, not with your documents. Try reloading in a moment.
			</Alert.Description>
		</Alert.Root>
	{:else}
		<DocumentTable documents={data.documents} />
	{/if}
</div>
