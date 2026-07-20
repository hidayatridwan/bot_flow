<script lang="ts">
	import ResetPasswordForm from '$lib/features/auth/components/reset-password-form.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
</script>

<svelte:head><title>Choose a new password</title></svelte:head>

<ResetPasswordForm data={data.form} hasToken={data.hasToken} />
