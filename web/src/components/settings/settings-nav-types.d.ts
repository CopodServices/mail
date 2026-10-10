import type { TranslationKey } from "@/lib/i18n/types";

type SettingsNavItem = {
	href: string;
	labelKey: TranslationKey;
};

export type SettingsNavSection = {
	labelKey: TranslationKey;
	items: SettingsNavItem[];
};
