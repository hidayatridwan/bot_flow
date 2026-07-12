<script lang="ts">
	import SignupForm from '$lib/features/auth/components/signup-form.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
</script>

<SignupForm data={data.form} />
