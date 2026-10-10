/**
 * Unit tests for the webmail session plumbing: the cookie helpers the API
 * client relies on and the sign-in call itself. No server is needed.
 */
import { beforeAll, beforeEach, describe, expect, test } from "bun:test";

type Utils = typeof import("@/lib/api/session-utils");
type Client = typeof import("@/lib/api/session");

let utils: Utils;
let client: Client;

beforeAll(async () => {
	// The API base is read when its module is first evaluated.
	process.env.NEXT_PUBLIC_MAIL_API_URL = "";
	utils = await import("@/lib/api/session-utils");
	client = await import("@/lib/api/session");
});

describe("session cookie helpers", () => {
	test("reads one cookie among many and decodes it", () => {
		const cookie = "a=1; cpod_mail_csrf=tok%2Ben; b=2";
		expect(utils.readCookie("cpod_mail_csrf", cookie)).toBe("tok+en");
		expect(utils.readCookie("missing", cookie)).toBeNull();
	});

	test("adds the CSRF header only to writes", () => {
		const cookie = "cpod_mail_csrf=tok";
		expect(utils.csrfHeaders("GET", cookie)).toEqual({});
		expect(utils.csrfHeaders("post", cookie)).toEqual({
			"X-CSRF-Token": "tok",
		});
		expect(utils.csrfHeaders("POST", "other=1")).toEqual({});
	});

	test("only honours same-origin return targets", () => {
		expect(utils.safeNext("/sent?message=1")).toBe("/sent?message=1");
		expect(utils.safeNext("//evil.test")).toBe("/inbox");
		expect(utils.safeNext("https://evil.test")).toBe("/inbox");
		expect(utils.loginHref("/drafts")).toBe("/login?next=%2Fdrafts");
		expect(utils.loginHref(null)).toBe("/login");
	});

	test("reports a session only when the readable CSRF cookie is present", () => {
		// The httpOnly session cookie alone is invisible to the client; the CSRF
		// cookie is the signal the realtime socket gates on.
		expect(
			utils.hasSession("cpod_mail_session=opaque; cpod_mail_csrf=tok"),
		).toBe(true);
		expect(utils.hasSession("cpod_mail_session=opaque")).toBe(false);
		expect(utils.hasSession("")).toBe(false);
	});
});

describe("signIn", () => {
	const originalFetch = globalThis.fetch;

	beforeEach(() => {
		globalThis.fetch = originalFetch;
	});

	test("posts the credentials with cookies and returns the mailbox", async () => {
		const calls: Array<{ url: string; init: RequestInit }> = [];
		globalThis.fetch = (async (url: unknown, init: RequestInit) => {
			calls.push({ url: String(url), init });
			return new Response(
				JSON.stringify({
					data: { mailbox_id: "m1", address: "a@b.test", domain: "b.test" },
				}),
				{ status: 200, headers: { "content-type": "application/json" } },
			);
		}) as typeof fetch;

		const mailbox = await client.signIn("a@b.test", "secret");
		const seen = calls[0];
		expect(mailbox.address).toBe("a@b.test");
		expect(seen.url).toBe("/api/session");
		expect(seen.init.method).toBe("POST");
		expect(seen.init.credentials).toBe("same-origin");
		expect(JSON.parse(String(seen.init.body))).toEqual({
			address: "a@b.test",
			password: "secret",
		});
	});

	test("maps a 401 to the invalid-credentials key", async () => {
		globalThis.fetch = (async () =>
			new Response("nope", { status: 401 })) as typeof fetch;
		await expect(client.signIn("a@b.test", "bad")).rejects.toThrow(
			"session.error.invalid",
		);
	});

	test("reports no session when the API answers 401", async () => {
		globalThis.fetch = (async () =>
			new Response("nope", { status: 401 })) as typeof fetch;
		expect(await client.fetchSession()).toBeNull();
	});
});
