<script lang="ts">
	import ForgotPasswordForm from '$lib/features/auth/components/forgot-password-form.svelte';
	import type { ActionData, PageData } from './$types';

	let { data, form }: { data: PageData; form: ActionData } = $props();
</script>

<svelte:head><title>Reset your password</title></svelte:head>

<ForgotPasswordForm data={data.form} sent={form?.sent ?? false} />
