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
	import { signupSchema, slugify, type SignupSchema } from '../schema';

	let {
		data,
		class: className,
		...restProps
	}: { data: SuperValidated<Infer<SignupSchema>> } & HTMLAttributes<HTMLDivElement> = $props();

	// superForm takes the initial value and owns the state from there — the reactive read is the point.
	// svelte-ignore state_referenced_locally
	const form = superForm(data, { validators: zod4Client(signupSchema) });
	const { form: formData, errors, enhance, submitting } = form;

	const formErrors = $derived($errors._errors ?? []);

	// The API derives a slug from the business name when one is not given, and its rules are
	// surprising — "Föö & Bar" becomes "f-bar", because non-ascii is dropped rather than
	// transliterated. Filling the field live lets the user see that before they submit, not after.
	// Once they edit the slug themselves we stop touching it.
	let slugTouched = $state(false);

	function onNameInput(event: Event & { currentTarget: HTMLInputElement }) {
		if (!slugTouched) $formData.slug = slugify(event.currentTarget.value);
	}
</script>

<div class={cn('flex flex-col gap-6', className)} {...restProps}>
	<Card.Root>
		<Card.Header class="text-center">
			<Card.Title class="text-xl">Create your account</Card.Title>
			<Card.Description>Enter your email below to create your account</Card.Description>
		</Card.Header>
		<Card.Content>
			<form method="POST" use:enhance>
				<FieldGroup>
					{#if formErrors.length > 0}
						<Alert.Root variant="destructive">
							<Alert.Description>{formErrors[0]}</Alert.Description>
						</Alert.Root>
					{/if}

					<Form.Field {form} name="name">
						<Form.Control>
							{#snippet children({ props })}
								<Form.Label>Business Name</Form.Label>
								<Input
									{...props}
									type="text"
									placeholder="John Doe"
									bind:value={$formData.name}
									oninput={onNameInput}
								/>
							{/snippet}
						</Form.Control>
						<Form.FieldErrors />
					</Form.Field>

					<Form.Field {form} name="slug">
						<Form.Control>
							{#snippet children({ props })}
								<Form.Label>Slug</Form.Label>
								<Input
									{...props}
									type="text"
									placeholder="john-doe"
									bind:value={$formData.slug}
									oninput={() => (slugTouched = true)}
								/>
							{/snippet}
						</Form.Control>
						<Form.Description>
							Lowercase letters, numbers and dashes. This cannot be changed later.
						</Form.Description>
						<Form.FieldErrors />
					</Form.Field>

					<Form.Field {form} name="email">
						<Form.Control>
							{#snippet children({ props })}
								<Form.Label>Email</Form.Label>
								<Input
									{...props}
									type="email"
									placeholder="m@example.com"
									bind:value={$formData.email}
								/>
							{/snippet}
						</Form.Control>
						<Form.FieldErrors />
					</Form.Field>

					<!-- A plain grid, not a nested Field: Field carries its own group/invalid styling, so
					     nesting one inside another makes error placement ambiguous. -->
					<div class="flex flex-col gap-2">
						<div class="grid grid-cols-2 gap-4">
							<Form.Field {form} name="password">
								<Form.Control>
									{#snippet children({ props })}
										<Form.Label>Password</Form.Label>
										<Input {...props} type="password" bind:value={$formData.password} />
									{/snippet}
								</Form.Control>
								<Form.FieldErrors />
							</Form.Field>

							<Form.Field {form} name="confirmPassword">
								<Form.Control>
									{#snippet children({ props })}
										<Form.Label>Confirm Password</Form.Label>
										<Input {...props} type="password" bind:value={$formData.confirmPassword} />
									{/snippet}
								</Form.Control>
								<Form.FieldErrors />
							</Form.Field>
						</div>
						<FieldDescription>Must be at least 8 characters long.</FieldDescription>
					</div>

					<div class="flex flex-col gap-2">
						<Form.Button disabled={$submitting} class="w-full">
							{#if $submitting}
								<Spinner />
							{/if}
							Create Account
						</Form.Button>
						<FieldDescription class="text-center">
							Already have an account? <a href={resolve('/login')}>Sign in</a>
						</FieldDescription>
					</div>
				</FieldGroup>
			</form>
		</Card.Content>
	</Card.Root>
	<FieldDescription class="px-6 text-center">
		By clicking continue, you agree to our <a href="#/">Terms of Service</a>
		and <a href="#/">Privacy Policy</a>.
	</FieldDescription>
</div>
