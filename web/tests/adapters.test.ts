/**
 * The adapter is the one place the wire shape turns into what the client
 * renders, so the fields the folder badges and the row state depend on are
 * pinned here: the delivery status is not the folder, a custom folder keeps its
 * id, a scheduled send keeps its time, and a copy the sender owns is read.
 */

import { describe, expect, test } from "bun:test";
import {
	adaptCounts,
	adaptMessageList,
	adaptThreadMessage,
} from "@/lib/api/adapters";

/** The wire shape the adapter accepts, taken from the adapter itself. */
type ApiMessageInput = NonNullable<
	Parameters<typeof adaptMessageList>[0]["data"]
>[number];

function apiMessage(overrides: Partial<ApiMessageInput> = {}): ApiMessageInput {
	return {
		id: "0194b7c1-0000-7000-8000-000000000001",
		mailbox_id: "0194b7c1-0000-7000-8000-0000000000aa",
		direction: "inbound",
		status: "stored",
		envelope_from: "sender@remote.test",
		envelope_to: "inbox@example.test",
		message_id: "<one@remote.test>",
		subject: "hello",
		header_from: "Sender <sender@remote.test>",
		size_bytes: 100,
		received_at: "2026-10-11T00:00:00Z",
		read: false,
		starred: false,
		folder: "inbox",
		...overrides,
	};
}

describe("message adapter", () => {
	test("keeps the delivery status apart from the folder", () => {
		const list = adaptMessageList({
			data: [apiMessage({ direction: "outbound", folder: "sent" })],
			meta: { total: 1, limit: 25, next_cursor: null },
		});
		const message = list.messages?.[0];
		expect(message?.status).toBe("stored");
		expect(message?.folder).toBe("sent");
	});

	test("a copy the sender owns is read, whatever its flag says", () => {
		const list = adaptMessageList({
			data: [
				apiMessage({ direction: "outbound", folder: "sent", read: false }),
			],
		});
		expect(list.messages?.[0].read).toBe(true);
	});

	test("a system folder has no folder id and a custom folder keeps its own", () => {
		const system = adaptMessageList({
			data: [apiMessage({ folder: "archived" })],
		});
		expect(system.messages?.[0].folderId).toBeNull();

		const custom = adaptMessageList({
			data: [apiMessage({ folder: "0194b7c1-0000-7000-8000-0000000000ff" })],
		});
		expect(custom.messages?.[0].folderId).toBe(
			"0194b7c1-0000-7000-8000-0000000000ff",
		);
	});

	test("carries the scheduled time and the delivery state", () => {
		const list = adaptMessageList({
			data: [
				apiMessage({
					folder: "scheduled",
					scheduled_at: "2026-10-12T08:00:00Z",
					send_state: "queued",
					send_error: null,
				}),
				apiMessage({
					folder: "sent",
					send_state: "failed",
					send_error: "550 rejected",
				}),
			],
		});
		expect(list.messages?.[0].scheduledAt).toBe("2026-10-12T08:00:00Z");
		expect(list.messages?.[0].sendState).toBe("queued");
		expect(list.messages?.[1].sendState).toBe("failed");
		expect(list.messages?.[1].sendError).toBe("550 rejected");
	});

	test("absence of state is null, not undefined", () => {
		const list = adaptMessageList({ data: [apiMessage()] });
		expect(list.messages?.[0].scheduledAt).toBeNull();
		expect(list.messages?.[0].sendState).toBeNull();
		expect(list.messages?.[0].sendError).toBeNull();
	});

	test("passes the cursor of the next page through", () => {
		const list = adaptMessageList({
			data: [apiMessage()],
			meta: { total: 60, limit: 25, next_cursor: "abc123" },
		});
		expect(list.nextCursor).toBe("abc123");
		expect(list.total).toBe(60);
		expect(list.limit).toBe(25);

		const last = adaptMessageList({ data: [], meta: { total: 0, limit: 25 } });
		expect(last.nextCursor).toBeNull();
	});

	test("falls back to the page length when the list carries no total", () => {
		// with_total=false: a plain folder view takes its total from /counts, so
		// the list response has no meta.total to read.
		const list = adaptMessageList({
			data: [
				apiMessage(),
				apiMessage({ id: "0194b7c1-0000-7000-8000-000000000002" }),
			],
			meta: { limit: 25 },
		});
		expect(list.total).toBe(2);
		expect(list.nextCursor).toBeNull();
	});

	test("a thread member carries no body until it is opened", () => {
		const member = adaptThreadMessage(apiMessage());
		expect(member.id).toBe("0194b7c1-0000-7000-8000-000000000001");
		expect(member.textBody).toBeNull();
		expect(member.htmlBody).toBeNull();
		expect(member.attachments).toEqual([]);
	});
});

describe("counts adapter", () => {
	test("always knows every system folder", () => {
		const counts = adaptCounts({
			data: { folders: { inbox: { total: 3, unread: 1 } } },
		});
		expect(counts.folders.inbox).toEqual({ total: 3, unread: 1 });
		for (const folder of [
			"starred",
			"snoozed",
			"sent",
			"scheduled",
			"drafts",
			"archived",
			"spam",
			"trash",
		] as const) {
			expect(counts.folders[folder]).toEqual({ total: 0, unread: 0 });
		}
	});

	test("keeps custom folders and mailboxes", () => {
		const counts = adaptCounts({
			data: {
				folders: { inbox: { total: 3, unread: 1 } },
				custom_folders: { "folder-1": { total: 2, unread: 2 } },
				mailboxes: [
					{
						mailbox_id: "0194b7c1-0000-7000-8000-0000000000aa",
						total: 5,
						unread: 3,
					},
				],
			},
		});
		expect(counts.customFolders["folder-1"]).toEqual({ total: 2, unread: 2 });
		expect(counts.mailboxes[0].unread).toBe(3);
	});
});
