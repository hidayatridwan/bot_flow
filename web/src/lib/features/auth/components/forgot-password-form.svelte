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
	import { forgotPasswordSchema, type ForgotPasswordSchema } from '../schema';

	let {
		data,
		sent = false,
		class: className,
		...restProps
	}: {
		data: SuperValidated<Infer<ForgotPasswordSchema>>;
		sent?: boolean;
	} & HTMLAttributes<HTMLDivElement> = $props();

	// svelte-ignore state_referenced_locally
	const form = superForm(data, { validators: zod4Client(forgotPasswordSchema) });
	const { form: formData, errors, enhance, submitting } = form;

	// Form-level only, like login: the API never tells us anything address-specific to attach.
	const formErrors = $derived($errors._errors ?? []);
</script>

<div class={cn('flex flex-col gap-6', className)} {...restProps}>
	<Card.Root>
		{#if sent}
			<!--
				The wording is careful on purpose. "If that address is registered" is not hedging — the
				API answers identically for a known and an unknown address so it cannot be used to
				discover which emails exist, and a confident "Check your inbox!" would either be false
				half the time or leak the difference by being shown only when it is true.
			-->
			<Card.Header class="text-center">
				<Card.Title class="text-xl">Check your email</Card.Title>
				<Card.Description>
					If that address is registered, a reset link is on its way. It expires in an hour and can
					be used once.
				</Card.Description>
			</Card.Header>
			<Card.Content>
				<FieldDescription class="text-center">
					Didn't get it? Check spam, or
					<a href={resolve('/forgot-password')}>try again</a>.
					<br />
					<a href={resolve('/login')}>Back to login</a>
				</FieldDescription>
			</Card.Content>
		{:else}
			<Card.Header class="text-center">
				<Card.Title class="text-xl">Reset your password</Card.Title>
				<Card.Description>
					Enter your email and we'll send you a link to choose a new password.
				</Card.Description>
			</Card.Header>
			<Card.Content>
				<form method="POST" use:enhance>
					<FieldGroup>
						{#if formErrors.length > 0}
							<Alert.Root variant="destructive">
								<Alert.Description>{formErrors[0]}</Alert.Description>
							</Alert.Root>
						{/if}

						<Form.Field {form} name="email">
							<Form.Control>
								{#snippet children({ props })}
									<Form.Label>Email</Form.Label>
									<Input
										{...props}
										type="email"
										autocomplete="email"
										placeholder="you@example.com"
										bind:value={$formData.email}
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
								Send reset link
							</Form.Button>
							<FieldDescription class="text-center">
								Remembered it? <a href={resolve('/login')}>Back to login</a>
							</FieldDescription>
						</div>
					</FieldGroup>
				</form>
			</Card.Content>
		{/if}
	</Card.Root>
</div>
