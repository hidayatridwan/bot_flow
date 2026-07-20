<script lang="ts">
	import { superForm } from 'sveltekit-superforms';
	import { zod4Client } from 'sveltekit-superforms/adapters';
	import * as Alert from '$lib/components/ui/alert/index.js';
	import * as Card from '$lib/components/ui/card/index.js';
	import { FieldGroup } from '$lib/components/ui/field/index.js';
	import * as Form from '$lib/components/ui/form/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import { Spinner } from '$lib/components/ui/spinner/index.js';
	import CircleCheckIcon from '@lucide/svelte/icons/circle-check';
	import { changePasswordSchema } from '$lib/features/auth/schema';
	import type { ActionData, PageData } from './$types';

	let { data, form: actionData }: { data: PageData; form: ActionData } = $props();

	// svelte-ignore state_referenced_locally
	const form = superForm(data.form, { validators: zod4Client(changePasswordSchema) });
	const { form: formData, errors, enhance, submitting } = form;

	const formErrors = $derived($errors._errors ?? []);
	const changed = $derived(actionData?.changed ?? false);
</script>

<svelte:head><title>Change password</title></svelte:head>

<div class="max-w-xl">
	<Card.Root>
		<Card.Header>
			<Card.Title>Change password</Card.Title>
			<Card.Description>
				Your current password is required even though you're signed in — a stolen session should not
				be enough to take the account.
			</Card.Description>
		</Card.Header>
		<Card.Content>
			{#if changed}
				<Alert.Root class="mb-4">
					<CircleCheckIcon />
					<Alert.Description>
						Password changed. Any other devices signed in to this account have been signed out.
					</Alert.Description>
				</Alert.Root>
			{/if}

			<form method="POST" use:enhance>
				<FieldGroup>
					{#if formErrors.length > 0}
						<Alert.Root variant="destructive">
							<Alert.Description>{formErrors[0]}</Alert.Description>
						</Alert.Root>
					{/if}

					<Form.Field {form} name="currentPassword">
						<Form.Control>
							{#snippet children({ props })}
								<Form.Label>Current password</Form.Label>
								<Input
									{...props}
									type="password"
									autocomplete="current-password"
									bind:value={$formData.currentPassword}
								/>
							{/snippet}
						</Form.Control>
						<Form.FieldErrors />
					</Form.Field>

					<Form.Field {form} name="newPassword">
						<Form.Control>
							{#snippet children({ props })}
								<Form.Label>New password</Form.Label>
								<Input
									{...props}
									type="password"
									autocomplete="new-password"
									bind:value={$formData.newPassword}
								/>
							{/snippet}
						</Form.Control>
						<Form.FieldErrors />
					</Form.Field>

					<Form.Field {form} name="confirmPassword">
						<Form.Control>
							{#snippet children({ props })}
								<Form.Label>Confirm new password</Form.Label>
								<Input
									{...props}
									type="password"
									autocomplete="new-password"
									bind:value={$formData.confirmPassword}
								/>
							{/snippet}
						</Form.Control>
						<Form.FieldErrors />
					</Form.Field>

					<Form.Button disabled={$submitting}>
						{#if $submitting}
							<Spinner />
						{/if}
						Change password
					</Form.Button>
				</FieldGroup>
			</form>
		</Card.Content>
	</Card.Root>
</div>
