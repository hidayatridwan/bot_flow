<script lang="ts">
	import { resolve } from '$app/paths';
	import { superForm, type Infer, type SuperValidated } from 'sveltekit-superforms';
	import { zod4Client } from 'sveltekit-superforms/adapters';
	import * as Alert from '$lib/components/ui/alert/index.js';
	import * as Card from '$lib/components/ui/card/index.js';
	import { FieldDescription, FieldGroup } from '$lib/components/ui/field/index.js';
	import * as Form from '$lib/components/ui/form/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import { Spinner } from '$lib/components/ui/spinner/index.js';
	import { cn } from '$lib/utils.js';
	import type { HTMLAttributes } from 'svelte/elements';
	import { resetPasswordSchema, type ResetPasswordSchema } from '../schema';

	let {
		data,
		hasToken = true,
		class: className,
		...restProps
	}: {
		data: SuperValidated<Infer<ResetPasswordSchema>>;
		hasToken?: boolean;
	} & HTMLAttributes<HTMLDivElement> = $props();

	// svelte-ignore state_referenced_locally
	const form = superForm(data, { validators: zod4Client(resetPasswordSchema) });
	const { form: formData, errors, enhance, submitting } = form;

	const formErrors = $derived($errors._errors ?? []);
</script>

<div class={cn('flex flex-col gap-6', className)} {...restProps}>
	<Card.Root>
		{#if !hasToken}
			<Card.Header class="text-center">
				<Card.Title class="text-xl">This link is incomplete</Card.Title>
				<Card.Description>
					It may have been cut short by your email client. Request a fresh one.
				</Card.Description>
			</Card.Header>
			<Card.Content>
				<FieldDescription class="text-center">
					<a href={resolve('/forgot-password')}>Send a new reset link</a>
				</FieldDescription>
			</Card.Content>
		{:else}
			<Card.Header class="text-center">
				<Card.Title class="text-xl">Choose a new password</Card.Title>
				<Card.Description>
					You'll be signed out everywhere else, then asked to log in with it.
				</Card.Description>
			</Card.Header>
			<Card.Content>
				<form method="POST" use:enhance>
					<FieldGroup>
						{#if formErrors.length > 0}
							<Alert.Root variant="destructive">
								<Alert.Description>
									{formErrors[0]}
									{#if formErrors[0]?.includes('expired') || formErrors[0]?.includes('invalid')}
										<!-- A dead link is the one error with an obvious next step, so it gets one
										     rather than leaving the user to find the login page themselves. -->
										<a href={resolve('/forgot-password')} class="underline">Request a new link</a>.
									{/if}
								</Alert.Description>
							</Alert.Root>
						{/if}

						<!-- The token travels in the form, not in the action URL: a redirect or a referer
						     would otherwise carry a live credential to wherever the browser goes next. -->
						<input type="hidden" name="token" bind:value={$formData.token} />

						<Form.Field {form} name="password">
							<Form.Control>
								{#snippet children({ props })}
									<Form.Label>New password</Form.Label>
									<Input
										{...props}
										type="password"
										autocomplete="new-password"
										bind:value={$formData.password}
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

						<div class="flex flex-col gap-2">
							<Form.Button disabled={$submitting} class="w-full">
								{#if $submitting}
									<Spinner />
								{/if}
								Set new password
							</Form.Button>
							<FieldDescription class="text-center">
								<a href={resolve('/login')}>Back to login</a>
							</FieldDescription>
						</div>
					</FieldGroup>
				</form>
			</Card.Content>
		{/if}
	</Card.Root>
</div>
