<script lang="ts">
	import LoginForm from '$lib/features/auth/components/login-form.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
</script>

<LoginForm data={data.form} />
