<script lang="ts" module>
	import AudioWaveformIcon from '@lucide/svelte/icons/audio-waveform';
	import ChartPieIcon from '@lucide/svelte/icons/chart-pie';
	import CommandIcon from '@lucide/svelte/icons/command';
	import FileTextIcon from '@lucide/svelte/icons/file-text';
	import FrameIcon from '@lucide/svelte/icons/frame';
	import GalleryVerticalEndIcon from '@lucide/svelte/icons/gallery-vertical-end';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import MapIcon from '@lucide/svelte/icons/map';
	import Settings2Icon from '@lucide/svelte/icons/settings-2';

	// This is sample data. `user` comes from the session (see the `user` prop below), and Dashboard
	// and Documents are real routes — the teams, the Config group and every remaining `#` are still
	// mocked. Nothing backs them yet.
	//
	// All three groups share one item shape (`title`/`url`/`icon`, optional `items`), because they
	// all render through NavGroup. An item WITHOUT `items` is a leaf: a plain link, no chevron.
	const data = {
		teams: [
			{
				name: 'Acme Inc',
				logo: GalleryVerticalEndIcon,
				plan: 'Enterprise'
			},
			{
				name: 'Acme Corp.',
				logo: AudioWaveformIcon,
				plan: 'Startup'
			},
			{
				name: 'Evil Corp.',
				logo: CommandIcon,
				plan: 'Free'
			}
		],
		navOverview: [
			{
				title: 'Dashboard',
				url: '/dashboard',
				icon: FileTextIcon
			}
		],
		navCore: [
			{
				title: 'Documents',
				url: '/documents',
				icon: FileTextIcon
			},
			{
				title: 'API keys',
				url: '/keys',
				icon: KeyRoundIcon
			},
			{
				title: 'Settings',
				url: '#',
				icon: Settings2Icon,
				items: [
					{
						title: 'General',
						url: '#'
					},
					{
						title: 'Team',
						url: '#'
					},
					{
						title: 'Billing',
						url: '#'
					},
					{
						title: 'Limits',
						url: '#'
					}
				]
			}
		],
		navConfig: [
			{
				title: 'Design Engineering',
				url: '#',
				icon: FrameIcon
			},
			{
				title: 'Sales & Marketing',
				url: '#',
				icon: ChartPieIcon
			},
			{
				title: 'Travel',
				url: '#',
				icon: MapIcon
			}
		]
	};
</script>

<script lang="ts">
	import NavGroup from './nav-group.svelte';
	import NavUser from './nav-user.svelte';
	import TeamSwitcher from './team-switcher.svelte';
	import * as DropdownMenu from '$lib/components/ui/dropdown-menu/index.js';
	import * as Sidebar from '$lib/components/ui/sidebar/index.js';
	import { useSidebar } from '$lib/components/ui/sidebar/context.svelte.js';
	import EllipsisIcon from '@lucide/svelte/icons/ellipsis';
	import FolderIcon from '@lucide/svelte/icons/folder';
	import ForwardIcon from '@lucide/svelte/icons/forward';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import type { SessionUser } from '$lib/types/auth';
	import type { ComponentProps } from 'svelte';

	let {
		ref = $bindable(null),
		collapsible = 'icon',
		user,
		...restProps
	}: ComponentProps<typeof Sidebar.Root> & { user: SessionUser } = $props();

	const sidebar = useSidebar();
</script>

<Sidebar.Root bind:ref {collapsible} {...restProps}>
	<Sidebar.Header>
		<TeamSwitcher teams={data.teams} />
	</Sidebar.Header>
	<Sidebar.Content>
		<NavGroup label="Overview" items={data.navOverview} />
		<NavGroup label="Core" items={data.navCore} />

		<!-- Config is the one group with per-item actions and a trailing row. They live here, as
		     snippets, rather than as flags on NavGroup — no other caller wants them. -->
		<NavGroup label="Config" items={data.navConfig} hideWhenCollapsed>
			{#snippet itemAction()}
				<DropdownMenu.Root>
					<DropdownMenu.Trigger>
						{#snippet child({ props })}
							<Sidebar.MenuAction showOnHover {...props}>
								<EllipsisIcon />
								<span class="sr-only">More</span>
							</Sidebar.MenuAction>
						{/snippet}
					</DropdownMenu.Trigger>
					<DropdownMenu.Content
						class="w-48 rounded-lg"
						side={sidebar.isMobile ? 'bottom' : 'right'}
						align={sidebar.isMobile ? 'end' : 'start'}
					>
						<DropdownMenu.Item>
							<FolderIcon class="text-muted-foreground" />
							<span>View Project</span>
						</DropdownMenu.Item>
						<DropdownMenu.Item>
							<ForwardIcon class="text-muted-foreground" />
							<span>Share Project</span>
						</DropdownMenu.Item>
						<DropdownMenu.Separator />
						<DropdownMenu.Item>
							<Trash2Icon class="text-muted-foreground" />
							<span>Delete Project</span>
						</DropdownMenu.Item>
					</DropdownMenu.Content>
				</DropdownMenu.Root>
			{/snippet}

			{#snippet footer()}
				<Sidebar.MenuItem>
					<Sidebar.MenuButton class="text-sidebar-foreground/70">
						<EllipsisIcon class="text-sidebar-foreground/70" />
						<span>More</span>
					</Sidebar.MenuButton>
				</Sidebar.MenuItem>
			{/snippet}
		</NavGroup>
	</Sidebar.Content>
	<Sidebar.Footer>
		<NavUser {user} />
	</Sidebar.Footer>
	<Sidebar.Rail />
</Sidebar.Root>
