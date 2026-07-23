<script lang="ts">
	import * as Sidebar from '$lib/components/ui/sidebar/index.js';
	import BotMessageSquareIcon from '@lucide/svelte/icons/bot-message-square';
	import type { SessionUser } from '$lib/types/auth';

	/**
	 * The tenant this session belongs to. Replaces the shadcn `TeamSwitcher`.
	 *
	 * **A switcher was not merely unpopulated — it was structurally wrong.** `accounts` carries
	 * `unique index idx_accounts_tenant` (migration 0009), so an account belongs to exactly one
	 * tenant and there is nothing to switch to, ever. Filling the old dropdown with real data would
	 * have produced a menu with one item and a chevron promising more.
	 *
	 * It used to offer *Acme Inc*, *Acme Corp.* and *Evil Corp.* on Enterprise/Startup/Free plans,
	 * rendered directly above the tenant's actual name. There are no plans in this product either.
	 */
	let { user }: { user: SessionUser } = $props();
</script>

<Sidebar.Menu>
	<Sidebar.MenuItem>
		<Sidebar.MenuButton size="lg" class="cursor-default hover:bg-transparent active:bg-transparent">
			{#snippet child({ props })}
				<div {...props}>
					<div
						class="bg-sidebar-primary text-sidebar-primary-foreground flex aspect-square size-8 items-center justify-center rounded-lg"
					>
						<BotMessageSquareIcon class="size-4" />
					</div>
					<div class="grid flex-1 text-start text-sm leading-tight">
						<span class="truncate font-medium">{user.tenantName}</span>
						<!-- The slug, not a plan tier. It is the value baked into object keys and the
						     Qdrant filter, so it is the one identifier a tenant may need to quote in a
						     support conversation. -->
						<span class="text-muted-foreground truncate text-xs">{user.tenantId}</span>
					</div>
				</div>
			{/snippet}
		</Sidebar.MenuButton>
	</Sidebar.MenuItem>
</Sidebar.Menu>
