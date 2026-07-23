<script lang="ts">
	import { enhance } from '$app/forms';
	import * as Avatar from '$lib/components/ui/avatar/index.js';
	import * as DropdownMenu from '$lib/components/ui/dropdown-menu/index.js';
	import * as Sidebar from '$lib/components/ui/sidebar/index.js';
	import { useSidebar } from '$lib/components/ui/sidebar/index.js';
	import { initialsFromEmail } from '$lib/features/auth/display';
	import type { SessionUser } from '$lib/types/auth';
	import { cn } from '$lib/utils.js';
	import ChevronsUpDownIcon from '@lucide/svelte/icons/chevrons-up-down';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import LogOutIcon from '@lucide/svelte/icons/log-out';

	// **Initials, not an image.** The API has no avatar to give, and the placeholder here used to be
	// `/avatars/shadcn.jpg` — a photograph of a real person (the shadcn author), rendered as every
	// tenant's own avatar. Initials derived from the signed-in email are real data and belong to the
	// person looking at them.
	let { user }: { user: SessionUser } = $props();

	const sidebar = useSidebar();
	const initials = $derived(initialsFromEmail(user.email));
</script>

<Sidebar.Menu>
	<Sidebar.MenuItem>
		<DropdownMenu.Root>
			<DropdownMenu.Trigger>
				{#snippet child({ props })}
					<Sidebar.MenuButton
						size="lg"
						class="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
						{...props}
					>
						<Avatar.Root class="size-8 rounded-lg">
							<Avatar.Fallback class="rounded-lg">{initials}</Avatar.Fallback>
						</Avatar.Root>
						<div class="grid flex-1 text-start text-sm leading-tight">
							<span class="truncate font-medium">{user.tenantName}</span>
							<span class="truncate text-xs">{user.email}</span>
						</div>
						<ChevronsUpDownIcon class="ms-auto size-4" />
					</Sidebar.MenuButton>
				{/snippet}
			</DropdownMenu.Trigger>
			<DropdownMenu.Content
				class="w-(--bits-dropdown-menu-anchor-width) min-w-56 rounded-lg"
				side={sidebar.isMobile ? 'bottom' : 'right'}
				align="end"
				sideOffset={4}
			>
				<DropdownMenu.Label class="p-0 font-normal">
					<div class="flex items-center gap-2 px-1 py-1.5 text-start text-sm">
						<Avatar.Root class="size-8 rounded-lg">
							<Avatar.Fallback class="rounded-lg">{initials}</Avatar.Fallback>
						</Avatar.Root>
						<div class="grid flex-1 text-start text-sm leading-tight">
							<span class="truncate font-medium">{user.tenantName}</span>
							<span class="truncate text-xs">{user.email}</span>
						</div>
					</div>
				</DropdownMenu.Label>
				<DropdownMenu.Separator />
				<!-- **Upgrade to Pro / Account / Billing / Notifications used to sit here, all inert.**
				     There is no billing in this product and no notifications; "Account" was the only one
				     that could have meant something, and what it would have meant is this link. -->
				<DropdownMenu.Group>
					<DropdownMenu.Item>
						{#snippet child({ props })}
							<a href="/settings/password" {...props}>
								<KeyRoundIcon />
								Change password
							</a>
						{/snippet}
					</DropdownMenu.Item>
				</DropdownMenu.Group>
				<DropdownMenu.Separator />
				<!-- The form lives *inside* the dropdown content: the content is portalled to <body>, so a
				     form wrapping the trigger would not contain this button. -->
				<form method="POST" action="/logout" use:enhance>
					<DropdownMenu.Item closeOnSelect={false}>
						{#snippet child({ props })}
							<button type="submit" {...props} class={cn(props.class as string, 'w-full')}>
								<LogOutIcon />
								Log out
							</button>
						{/snippet}
					</DropdownMenu.Item>
				</form>
			</DropdownMenu.Content>
		</DropdownMenu.Root>
	</Sidebar.MenuItem>
</Sidebar.Menu>
