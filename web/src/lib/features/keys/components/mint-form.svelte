<script lang="ts">
	import { superForm, type SuperValidated, type Infer } from 'sveltekit-superforms';
	import { zod4Client } from 'sveltekit-superforms/adapters';
	import * as Alert from '$lib/components/ui/alert/index.js';
	import * as Card from '$lib/components/ui/card/index.js';
	import * as Form from '$lib/components/ui/form/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import { Textarea } from '$lib/components/ui/textarea/index.js';
	import { Spinner } from '$lib/components/ui/spinner/index.js';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import { mintSchema, type MintSchema } from '../schema';

	let { data }: { data: SuperValidated<Infer<MintSchema>> } = $props();

	// superForm takes the initial value and owns the state from there — the reactive read is the point.
	// svelte-ignore state_referenced_locally
	const form = superForm(data, {
		validators: zod4Client(mintSchema),
		// Clear the inputs on success. The reveal survives because it comes from the action result, not
		// from this store — and leaving the fields filled invites a second click that mints a duplicate
		// key the tenant never wanted.
		resetForm: true,
		invalidateAll: true
	});
	const { form: formData, enhance, submitting, errors } = form;

	const formErrors = $derived($errors._errors ?? []);
	const isPublishable = $derived($formData.kind === 'publishable');
</script>

<Card.Root>
	<Card.Header>
		<Card.Title>Create a key</Card.Title>
		<Card.Description>
			A publishable key goes in your web page and can only ask questions. A secret key stays on your
			server and can do everything.
		</Card.Description>
	</Card.Header>
	<Card.Content>
		<form method="POST" action="?/mint" use:enhance class="flex flex-col gap-4">
			{#if formErrors.length > 0}
				<Alert.Root variant="destructive">
					<CircleAlertIcon />
					<Alert.Description>{formErrors[0]}</Alert.Description>
				</Alert.Root>
			{/if}

			<Form.Field {form} name="kind">
				<Form.Control>
					{#snippet children({ props })}
						<Form.Label>Type</Form.Label>
						<select
							{...props}
							bind:value={$formData.kind}
							class="border-input bg-background h-9 rounded-md border px-3 text-sm"
						>
							<option value="publishable">Publishable — for your website widget</option>
							<option value="secret">Secret — for your server</option>
						</select>
					{/snippet}
				</Form.Control>
				<Form.FieldErrors />
			</Form.Field>

			<Form.Field {form} name="label">
				<Form.Control>
					{#snippet children({ props })}
						<Form.Label>Label</Form.Label>
						<Input {...props} bind:value={$formData.label} placeholder="marketing site" />
					{/snippet}
				</Form.Control>
				<Form.Description>Just for you — it helps you tell keys apart later.</Form.Description>
				<Form.FieldErrors />
			</Form.Field>

			<Form.Field {form} name="origins">
				<Form.Control>
					{#snippet children({ props })}
						<Form.Label>Allowed origins</Form.Label>
						<Textarea
							{...props}
							bind:value={$formData.origins}
							rows={3}
							placeholder={'https://example.com\nhttps://www.example.com'}
						/>
					{/snippet}
				</Form.Control>
				<Form.Description>
					{#if isPublishable}
						One per line, as <code>scheme://host</code> — exactly how a browser sends it. Required: a
						publishable key with no origins cannot answer from anywhere.
					{:else}
						Ignored for secret keys — they are never origin-checked.
					{/if}
				</Form.Description>
				<Form.FieldErrors />
			</Form.Field>

			<div>
				<Form.Button disabled={$submitting}>
					{#if $submitting}<Spinner />{/if}
					Create key
				</Form.Button>
			</div>
		</form>
	</Card.Content>
</Card.Root>
