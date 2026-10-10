/**
 * Keeps the translation catalogs honest. English ships in the bundle; every
 * other locale is a static catalog under public/locales. What must hold: every
 * registered locale has a catalog, every catalog covers every English key, no
 * catalog invents a key, and no catalog invents a placeholder.
 *
 * A translation may drop a placeholder on purpose (Arabic writes "one
 * attachment" without the number), but an invented one would render literally
 * in the UI, so that is an error.
 */

import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import en from "@/lib/i18n/en.json";
import { locales } from "@/lib/i18n/locales";

const english = en as Record<string, string>;
const dir = join(
	dirname(fileURLToPath(import.meta.url)),
	"..",
	"public",
	"locales",
);
const files = readdirSync(dir).filter((name) => name.endsWith(".json"));
const catalogs = files.map((name) => ({
	code: name.replace(/\.json$/, ""),
	entries: JSON.parse(readFileSync(join(dir, name), "utf8")) as Record<
		string,
		string
	>,
}));

/** The {name} tokens a catalog string interpolates. */
function placeholders(value: string): string[] {
	return [...value.matchAll(/\{(\w+)\}/g)].map((match) => match[1]);
}

describe("translation catalogs", () => {
	test("every registered locale has a catalog, and nothing else does", () => {
		const shipped = new Set(files.map((name) => name.replace(/\.json$/, "")));
		const expected = Object.keys(locales).filter((code) => code !== "en");
		expect(expected.filter((code) => !shipped.has(code))).toEqual([]);
		expect([...shipped].filter((code) => !(code in locales))).toEqual([]);
	});

	test("no catalog carries a key English does not have", () => {
		const unknown = catalogs.flatMap((catalog) =>
			Object.keys(catalog.entries)
				.filter((key) => !(key in english))
				.map((key) => catalog.code + ":" + key),
		);
		expect(unknown).toEqual([]);
	});

	test("every catalog covers every English key", () => {
		const incomplete = catalogs
			.map((catalog) => ({
				code: catalog.code,
				missing: Object.keys(english).filter(
					(key) => !(key in catalog.entries),
				),
			}))
			.filter((entry) => entry.missing.length > 0)
			.map((entry) => entry.code + " is missing " + entry.missing.join(", "));
		expect(incomplete).toEqual([]);
	});

	test("no catalog invents a placeholder", () => {
		const unknown = catalogs.flatMap((catalog) =>
			Object.entries(catalog.entries)
				.filter(([key, value]) =>
					placeholders(value).some(
						(name) => !placeholders(english[key]).includes(name),
					),
				)
				.map(([key]) => catalog.code + ":" + key),
		);
		expect(unknown).toEqual([]);
	});
});
