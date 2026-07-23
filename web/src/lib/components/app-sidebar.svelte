<script lang="ts" module>
	import BotMessageSquareIcon from '@lucide/svelte/icons/bot-message-square';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import LayoutDashboardIcon from '@lucide/svelte/icons/layout-dashboard';
	import Settings2Icon from '@lucide/svelte/icons/settings-2';

	// **Every entry here is a route that exists.** That is the rule, and it is worth stating because
	// this file used to break it in three ways at once: a "Config" group of shadcn sample items
	// (Design Engineering / Sales & Marketing / Travel) pointing at `#`, a Settings submenu offering
	// **Team** and **Billing** to a product that has neither, and a tenant switcher listing three
	// fictional companies on three fictional plans.
	//
	// None of it lost data or leaked anything, which is exactly why it survived so long. It cost
	// something harder to see: a tenant who clicks Billing and finds nothing learns that this UI does
	// not mean what it says, and after that the parts that *are* true have to earn belief separately.
	//
	// A short honest menu beats a long aspirational one. Add an entry when its page lands, not before.
	//
	// Both groups share one item shape (`title`/`url`/`icon`, optional `items`), because they render
	// through NavGroup. An item WITHOUT `items` is a leaf: a plain link, no chevron.
	const data = {
		navOverview: [
			{
				title: 'Dashboard',
				url: '/dashboard',
				icon: LayoutDashboardIcon
			}
		],
		navCore: [
			{
				title: 'Documents',
				url: '/documents',
				icon: FileTextIcon
			},
			{
				title: 'Playground',
				url: '/playground',
				icon: BotMessageSquareIcon
			},
			{
				title: 'API keys',
				url: '/keys',
				icon: KeyRoundIcon
			},
			{
				title: 'Settings',
				url: '/settings/password',
				icon: Settings2Icon,
				items: [
					{
						title: 'Password',
						url: '/settings/password'
					}
				]
			}
		]
	};
</script>

<script lang="ts">
	import NavGroup from './nav-group.svelte';
	import NavUser from './nav-user.svelte';
	import TenantBadge from './tenant-badge.svelte';
	import * as Sidebar from '$lib/components/ui/sidebar/index.js';
	import type { SessionUser } from '$lib/types/auth';
	import type { ComponentProps } from 'svelte';

	let {
		ref = $bindable(null),
		collapsible = 'icon',
		user,
		...restProps
	}: ComponentProps<typeof Sidebar.Root> & { user: SessionUser } = $props();
</script>

<Sidebar.Root bind:ref {collapsible} {...restProps}>
	<Sidebar.Header>
		<TenantBadge {user} />
	</Sidebar.Header>
	<Sidebar.Content>
		<NavGroup label="Overview" items={data.navOverview} />
		<NavGroup label="Core" items={data.navCore} />
	</Sidebar.Content>
	<Sidebar.Footer>
		<NavUser {user} />
	</Sidebar.Footer>
	<Sidebar.Rail />
</Sidebar.Root>
