/**
 * The mailbox list feeds the composer's From header. A mailbox without a stored
 * display name falls back to its local part, but a stored one must win, or the
 * recipient sees the address-derived name instead of the name the sender set.
 */
import { expect, test } from "bun:test";

test("a stored display name wins over the local-part fallback", async () => {
	process.env.NEXT_PUBLIC_MAIL_API_URL = "";
	const original = globalThis.fetch;
	globalThis.fetch = (async () =>
		new Response(
			JSON.stringify({
				data: [
					{
						id: "m1",
						address: "info@example.test",
						active: true,
						signature: null,
						display_name: "Mandala Karya Internasional",
					},
					{
						id: "m2",
						address: "sales@example.test",
						active: true,
						signature: null,
						display_name: null,
					},
				],
			}),
			{ status: 200, headers: { "content-type": "application/json" } },
		)) as typeof fetch;
	try {
		const { fetchMailboxOptions } = await import(
			"@/components/mailbox-provider-utils"
		);
		const options = await fetchMailboxOptions(true);
		expect(options.map((option) => option.displayName)).toEqual([
			"Mandala Karya Internasional",
			"sales",
		]);
	} finally {
		globalThis.fetch = original;
	}
});
