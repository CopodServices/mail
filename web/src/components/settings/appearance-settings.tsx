"use client";

import { useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import {
	isThemePreference,
	readThemePreference,
	saveThemePreference,
	THEME_CHANGED_EVENT,
	type ThemePreference,
} from "@/components/theme-utils";
import { Select } from "@/components/ui/select";
import { locales } from "@/lib/i18n/locales";
import type { Locale, TranslationKey } from "@/lib/i18n/types";

const OPTIONS: { value: ThemePreference; labelKey: TranslationKey }[] = [
	{ value: "system", labelKey: "settings.theme.system" },
	{ value: "light", labelKey: "settings.theme.light" },
	{ value: "dark", labelKey: "settings.theme.dark" },
];

/** The languages this client ships, in registry order, by their native name. */
const LOCALE_OPTIONS = (Object.keys(locales) as Locale[]).map((code) => ({
	code,
	label: locales[code].label,
}));

export function AppearanceSettings() {
	const { t, locale, setLocale } = useLanguage();
	const [preference, setPreference] = useState<ThemePreference>("system");

	useEffect(() => {
		setPreference(readThemePreference());
		const sync = () => setPreference(readThemePreference());
		window.addEventListener(THEME_CHANGED_EVENT, sync);
		return () => window.removeEventListener(THEME_CHANGED_EVENT, sync);
	}, []);

	return (
		<div className="space-y-3">
			<label className="flex items-start gap-3 rounded-xl bg-neutral-50 p-4">
				<span className="flex-1">
					<span className="block text-sm font-medium text-neutral-900">
						{t("settings.theme.label")}
					</span>
					<span className="mt-1 block text-sm text-neutral-500">
						{t("settings.theme.description")}
					</span>
				</span>
				<Select
					value={preference}
					aria-label={t("settings.theme.label")}
					className="py-1.5 text-sm text-neutral-900"
					onChange={(event) => {
						const next = event.target.value;
						if (!isThemePreference(next)) return;
						setPreference(next);
						saveThemePreference(next);
					}}
				>
					{OPTIONS.map((option) => (
						<option key={option.value} value={option.value}>
							{t(option.labelKey)}
						</option>
					))}
				</Select>
			</label>
			<label className="flex items-start gap-3 rounded-xl bg-neutral-50 p-4">
				<span className="flex-1">
					<span className="block text-sm font-medium text-neutral-900">
						{t("language.label")}
					</span>
				</span>
				<Select
					value={locale}
					aria-label={t("language.label")}
					className="py-1.5 text-sm text-neutral-900"
					onChange={(event) => setLocale(event.target.value as Locale)}
				>
					{LOCALE_OPTIONS.map((option) => (
						<option key={option.code} value={option.code}>
							{option.label}
						</option>
					))}
				</Select>
			</label>
		</div>
	);
}
