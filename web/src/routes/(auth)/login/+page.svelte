<script lang="ts">
	import { page } from '$app/state';
	import LoginForm from '$lib/features/auth/components/login-form.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();

	// Set by the reset page's redirect. A query flag rather than a flash cookie because it carries no
	// secret and survives a refresh harmlessly — the opposite of the one-time `sk_` reveal.
	const justReset = $derived(page.url.searchParams.get('reset') === '1');
</script>

<svelte:head><title>Log in</title></svelte:head>

<LoginForm data={data.form} {justReset} />
