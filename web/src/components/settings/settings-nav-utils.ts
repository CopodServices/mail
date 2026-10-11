import type { SettingsNavSection } from "./settings-nav-types";

export const settingsNavSections: SettingsNavSection[] = [
	{
		labelKey: "settings.nav.settings",
		items: [
			{
				href: "/settings/inbox",
				labelKey: "settings.nav.inbox",
			},
			{
				href: "/settings/forwarding",
				labelKey: "settings.nav.forwarding",
			},
			{
				href: "/settings/clients",
				labelKey: "settings.nav.clients",
			},
			{
				href: "/settings/spam",
				labelKey: "settings.nav.spam",
			},
		],
	},
];

export function isActiveSettingsPath(pathname: string, href: string): boolean {
	return pathname === href || pathname.startsWith(href + "/");
}
