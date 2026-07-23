<script lang="ts">
	import { resolve } from '$app/paths';
	import { Button } from '$lib/components/ui/button/index.js';
	import BotMessageSquareIcon from '@lucide/svelte/icons/bot-message-square';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import QuoteIcon from '@lucide/svelte/icons/quote';
	import ShieldCheckIcon from '@lucide/svelte/icons/shield-check';

	/**
	 * The page a cold self-serve visitor lands on. It used to be an unstyled `<h1>` and two bare links.
	 *
	 * **Everything stated here is something the system actually does**, which for a landing page is a
	 * discipline rather than a given: no pricing (there is no billing), no customer logos, no uptime
	 * figure, no "trusted by" line. The three claims below map to real, documented behaviour —
	 * grounded answers with a refusal path (invariant 4), citations returned as structured data
	 * (invariant 5), and per-tenant isolation across all three stores.
	 */
	const features = [
		{
			icon: FileTextIcon,
			title: 'Answers from your documents, or not at all',
			body: 'Upload PDFs, text or Markdown. When nothing in them is relevant, the bot says so instead of guessing — a wrong refund policy is worse than an honest "I don\'t know".'
		},
		{
			icon: QuoteIcon,
			title: 'Every answer carries its sources',
			body: 'Each reply comes back with the passages it was drawn from, so you can check it against the original before your customers see it.'
		},
		{
			icon: ShieldCheckIcon,
			title: 'Your content stays yours',
			body: 'Each workspace is isolated in the database, the vector store and object storage. Delete a document and it goes from all three.'
		}
	];
</script>

<svelte:head>
	<title>BotFlow — a support bot that only answers from your documents</title>
	<meta
		name="description"
		content="Upload your support documents and get an embeddable chat widget that answers from them, with citations — and declines when it does not know."
	/>
</svelte:head>

<div class="flex min-h-svh flex-col">
	<header class="flex items-center justify-between p-6">
		<span class="flex items-center gap-2 font-medium">
			<span
				class="bg-primary text-primary-foreground flex size-6 items-center justify-center rounded-md"
			>
				<BotMessageSquareIcon class="size-4" />
			</span>
			BotFlow
		</span>
		<nav class="flex items-center gap-2">
			<Button href={resolve('/login')} variant="ghost" size="sm">Log in</Button>
			<Button href={resolve('/signup')} size="sm">Sign up</Button>
		</nav>
	</header>

	<main class="mx-auto flex w-full max-w-3xl flex-1 flex-col justify-center gap-12 px-6 py-16">
		<div class="flex flex-col gap-6">
			<h1 class="text-4xl font-semibold tracking-tight text-balance sm:text-5xl">
				A support bot that only answers from your own documents.
			</h1>
			<p class="text-muted-foreground max-w-2xl text-lg text-pretty">
				Upload what your team already writes — handbooks, policies, FAQs — and embed a chat widget
				that answers from them and cites what it used. When the answer isn't in your documents, it
				says so.
			</p>
			<div class="flex flex-wrap items-center gap-3">
				<Button href={resolve('/signup')} size="lg">Create a workspace</Button>
				<Button href={resolve('/login')} variant="outline" size="lg">Log in</Button>
			</div>
		</div>

		<div class="grid gap-8 sm:grid-cols-3">
			{#each features as feature (feature.title)}
				<div class="flex flex-col gap-2">
					<feature.icon class="text-muted-foreground size-5" />
					<h2 class="font-medium">{feature.title}</h2>
					<p class="text-muted-foreground text-sm text-pretty">{feature.body}</p>
				</div>
			{/each}
		</div>
	</main>

	<footer class="text-muted-foreground p-6 text-sm">BotFlow</footer>
</div>
