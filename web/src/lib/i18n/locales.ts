type LocaleEntry = {
	/** The language's own name, shown in the selector. */
	label: string;
	/** Set for right-to-left scripts. */
	dir?: "rtl";
};

// Register a language here with its native name, and add its catalog as `public/locales/<code>.json`.
// English ships in the bundle; other catalogs are static assets fetched when first needed, so they
// do not count toward the Worker size limit.
// Types, validation, cookies, SSR and the selector all derive from this registry.
export const locales = {
	en: { label: "English" },
	id: { label: "Bahasa Indonesia" },
} satisfies Record<string, LocaleEntry>;

export type Locale = keyof typeof locales;
export const DEFAULT_LOCALE = "en" satisfies Locale;
