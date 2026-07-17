<script lang="ts" module>
	// This should be `Component` after @lucide/svelte updates types.
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	type Icon = any;

	export type NavItem = {
		title: string;
		url: string;
		icon?: Icon;
		/** Branches only: start the collapsible open. */
		isActive?: boolean;
		/** Present and non-empty ⇒ this is a branch, and gets a collapsible + chevron. */
		items?: { title: string; url: string }[];
	};
</script>

<script lang="ts">
	import { page } from '$app/state';
	import * as Collapsible from '$lib/components/ui/collapsible/index.js';
	import * as Sidebar from '$lib/components/ui/sidebar/index.js';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import { isCurrentPath } from '$lib/utils/nav';
	import type { Snippet } from 'svelte';

	/**
	 * One sidebar group, for both shapes of nav item:
	 *
	 * - a **leaf** (no `items`) is a plain link, with **no chevron** — a chevron promises a submenu,
	 *   and one that expands into nothing is a lie. It also has to be an `<a href>`: the whole reason
	 *   this component exists is that the previous version rendered every item as a collapsible
	 *   trigger, so `item.url` went unused and leaf items simply did not navigate.
	 * - a **branch** (has `items`) keeps the collapsible and the chevron.
	 *
	 * The per-item action and the trailing row are snippets rather than boolean props: only the Config
	 * group wants them, and a component that grows a flag per caller is worse than the duplication it
	 * replaced.
	 */
	let {
		label,
		items,
		hideWhenCollapsed = false,
		itemAction,
		footer
	}: {
		label: string;
		items: NavItem[];
		/** The Config group hides rather than squeeze into the icon rail. */
		hideWhenCollapsed?: boolean;
		itemAction?: Snippet<[NavItem]>;
		footer?: Snippet;
	} = $props();
</script>

<Sidebar.Group class={hideWhenCollapsed ? 'group-data-[collapsible=icon]:hidden' : undefined}>
	<Sidebar.GroupLabel>{label}</Sidebar.GroupLabel>
	<Sidebar.Menu>
		{#each items as item (item.title)}
			<!-- `?.length`, not `?.items`: an empty array is a leaf, not a branch with nothing in it. -->
			{#if item.items?.length}
				<Collapsible.Root open={item.isActive} class="group/collapsible">
					{#snippet child({ props })}
						<Sidebar.MenuItem {...props}>
							<Collapsible.Trigger>
								{#snippet child({ props })}
									<Sidebar.MenuButton {...props} tooltipContent={item.title}>
										{#if item.icon}
											<item.icon />
										{/if}
										<span>{item.title}</span>
										<ChevronRightIcon
											class="ms-auto transition-transform duration-200 group-data-[state=open]/collapsible:rotate-90"
										/>
									</Sidebar.MenuButton>
								{/snippet}
							</Collapsible.Trigger>
							<Collapsible.Content>
								<Sidebar.MenuSub>
									{#each item.items ?? [] as subItem (subItem.title)}
										<Sidebar.MenuSubItem>
											<Sidebar.MenuSubButton>
												{#snippet child({ props })}
													<a href={subItem.url} {...props}>
														<span>{subItem.title}</span>
													</a>
												{/snippet}
											</Sidebar.MenuSubButton>
										</Sidebar.MenuSubItem>
									{/each}
								</Sidebar.MenuSub>
							</Collapsible.Content>
						</Sidebar.MenuItem>
					{/snippet}
				</Collapsible.Root>
			{:else}
				<Sidebar.MenuItem>
					<Sidebar.MenuButton
						isActive={isCurrentPath(item.url, page.url.pathname)}
						tooltipContent={item.title}
					>
						{#snippet child({ props })}
							<a href={item.url} {...props}>
								{#if item.icon}
									<item.icon />
								{/if}
								<span>{item.title}</span>
							</a>
						{/snippet}
					</Sidebar.MenuButton>
					{@render itemAction?.(item)}
				</Sidebar.MenuItem>
			{/if}
		{/each}
		{@render footer?.()}
	</Sidebar.Menu>
</Sidebar.Group>
