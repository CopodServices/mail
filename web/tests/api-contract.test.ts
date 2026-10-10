/**
 * Runs the client's own API helpers and adapters against a live mail API.
 *
 * Set MAIL_API_URL to enable it; without it the suite is skipped, so the
 * normal build and lint never need a server:
 *
 *     MAIL_API_URL=http://127.0.0.1:8080 bun test
 */
import { beforeAll, describe, expect, test } from "bun:test";

const base = process.env.MAIL_API_URL?.trim().replace(/\/+$/, "");
// The client reads this when its API base module is first imported.
process.env.NEXT_PUBLIC_MAIL_API_URL = base ?? "";

type Client = typeof import("@/lib/api/endpoints");
type Adapt = typeof import("@/lib/api/adapters");

async function readJson(path: string) {
	const response = await fetch((base ?? "") + path);
	expect(response.ok).toBe(true);
	return response.json();
}

describe.skipIf(!base)("the web client against the live API", () => {
	let api: Client;
	let adapters: Adapt;
	let mailboxId: string;
	let mailboxAddress: string;

	beforeAll(async () => {
		api = await import("@/lib/api/endpoints");
		adapters = await import("@/lib/api/adapters");
		const mailboxes = await readJson("/api/mailboxes");
		mailboxId = mailboxes.data[0].id;
		mailboxAddress = mailboxes.data[0].address;
	});

	test("lists the system folders and creates a custom one", async () => {
		const before = await api.fetchFolders(mailboxId);
		expect(before.filter((folder) => folder.kind === "system")).toHaveLength(9);

		const name = "Contract " + Date.now();
		const created = await api.createFolder(mailboxId, name, "#123456");
		expect(created.kind).toBe("custom");
		expect(created.color).toBe("#123456");

		const after = await api.fetchFolders(mailboxId);
		expect(after.some((folder) => folder.id === created.id)).toBe(true);
	});

	test("saves, reloads, attaches to and discards a draft", async () => {
		const id = await api.saveDraft({
			mailbox_id: mailboxId,
			from: mailboxAddress,
			to: "bob@remote.test",
			subject: "contract draft",
			text: "body",
			html: "<p>body</p>",
		});
		expect(typeof id).toBe("string");

		const loaded = await api.loadDraft(id);
		expect(loaded.subject).toBe("contract draft");
		expect(loaded.to).toBe("bob@remote.test");
		expect(loaded.attachments).toHaveLength(0);

		const file = new File(["attachment-bytes"], "note.txt", {
			type: "text/plain",
		});
		const attachments = await api.uploadDraftAttachments(id, [file]);
		expect(attachments.map((item) => item.filename)).toEqual(["note.txt"]);

		const reloaded = await api.loadDraft(id);
		expect(reloaded.attachments).toHaveLength(1);

		const removed = await api.removeDraftAttachment(id, "0");
		expect(removed).toHaveLength(0);

		await api.deleteDraft(id);
		await expect(api.loadDraft(id)).rejects.toThrow();
	});

	test("sends now and schedules for later", async () => {
		const now = await api.sendMessage({
			from: mailboxAddress,
			to: [mailboxAddress],
			subject: "contract immediate",
			text: "body",
		});
		expect(now.scheduled).not.toBe(true);

		const later = await api.sendMessage({
			from: mailboxAddress,
			to: [mailboxAddress],
			subject: "contract scheduled",
			text: "body",
			scheduledAt: new Date(Date.now() + 3_600_000).toISOString(),
		});
		expect(later.scheduled).toBe(true);
	});

	test("a mailbox signature reaches the mailbox the composer reads", async () => {
		const { fetchMailboxOptions } = await import(
			"@/components/mailbox-provider-utils"
		);
		const signature = "Regards," + String.fromCharCode(10) + "QA";
		const saved = await fetch(base + "/api/mailboxes/" + mailboxAddress, {
			method: "PATCH",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ signature }),
		});
		expect(saved.ok).toBe(true);

		const options = await fetchMailboxOptions(true);
		expect(options.find((box) => box.id === mailboxId)?.signature).toBe(
			signature,
		);
		await fetch(base + "/api/mailboxes/" + mailboxAddress, {
			method: "PATCH",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ signature: null }),
		});
	});

	test("a search the client builds filters the list", async () => {
		const { getMessageQueryParams, fetchMessageList } = await import(
			"@/hooks/utils"
		);
		const needle = "Needle " + Date.now();
		const draftId = await api.saveDraft({
			mailbox_id: mailboxId,
			from: mailboxAddress,
			subject: needle,
			text: "searchable body",
		});

		// The client builds the API's parameter names, not its own.
		const params = getMessageQueryParams("drafts", mailboxId, {
			query: needle,
		});
		expect(params.get("folder")).toBe("drafts");
		expect(params.get("mailbox_id")).toBe(mailboxId);
		expect(params.get("q")).toBe(needle);

		const page = await fetchMessageList(params, true);
		expect(page.messages?.some((message) => message.id === draftId)).toBe(true);
		expect(
			page.messages?.every((message) =>
				(message.subject ?? "").toLowerCase().includes(needle.toLowerCase()),
			),
		).toBe(true);

		await api.deleteDraft(draftId);
	});

	test("adapters map live payloads into the client's model", async () => {
		const list = adapters.adaptMessageList(
			await readJson("/api/messages?limit=5&mailbox_id=" + mailboxId),
		);
		expect(Array.isArray(list.messages)).toBe(true);
		expect(typeof list.total).toBe("number");
		for (const message of list.messages ?? []) {
			expect(typeof message.id).toBe("string");
			expect(typeof message.read).toBe("boolean");
			expect(
				message.direction === "inbound" || message.direction === "outbound",
			).toBe(true);
			expect(typeof message.folder).toBe("string");
			expect(
				message.folderId === null || typeof message.folderId === "string",
			).toBe(true);
		}
		expect(
			list.nextCursor === null ||
				list.nextCursor === undefined ||
				typeof list.nextCursor === "string",
		).toBe(true);

		const counts = adapters.adaptCounts(
			await readJson("/api/messages/counts?mailbox_id=" + mailboxId),
		);
		expect(Object.keys(counts.folders).sort()).toEqual(
			[
				"archived",
				"drafts",
				"inbox",
				"scheduled",
				"sent",
				"snoozed",
				"spam",
				"starred",
				"trash",
			].sort(),
		);
		for (const folder of Object.values(counts.folders)) {
			expect(typeof folder.total).toBe("number");
			expect(typeof folder.unread).toBe("number");
		}
	});

	test("pages the list with the cursor a page returned", async () => {
		const { getMessageQueryParams, fetchMessageList } = await import(
			"@/hooks/utils"
		);
		const first = await fetchMessageList(
			getMessageQueryParams("inbox", mailboxId, { limit: 1 }),
			true,
		);
		expect((first.messages ?? []).length).toBeLessThanOrEqual(1);
		if (first.nextCursor) {
			const second = await fetchMessageList(
				getMessageQueryParams("inbox", mailboxId, {
					limit: 1,
					cursor: first.nextCursor,
				}),
				true,
			);
			expect(second.messages?.[0]?.id).not.toBe(first.messages?.[0]?.id);
			// The resume point is a value, so the page after it excludes it.
			expect(
				(second.messages ?? []).some(
					(message) => message.id === first.messages?.[0]?.id,
				),
			).toBe(false);
		}

		// A cursor the API did not write is refused rather than guessed at.
		const refused = await fetch(
			(base ?? "") + "/api/messages?limit=1&cursor=nope",
		);
		expect(refused.status).toBe(400);
	});
});
