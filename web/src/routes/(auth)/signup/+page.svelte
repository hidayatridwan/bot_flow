<script lang="ts">
	import SignupForm from '$lib/features/auth/components/signup-form.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
</script>

<svelte:head><title>Create your workspace</title></svelte:head>

<SignupForm data={data.form} />
