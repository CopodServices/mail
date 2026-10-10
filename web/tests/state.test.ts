/**
 * The folder badges, the optimistic unread delta and the row state labels are
 * pure functions, so the rules they encode are pinned here: a copy the sender
 * owns is never unread, Drafts and Scheduled count what is waiting, Sent shows
 * nothing, and a read in one folder never moves another folder's badge.
 */

import { describe, expect, test } from "bun:test";
import { getFolderBadgeCount } from "@/components/dashboard-nav-utils";
import { getPageRange, getSendStateKey } from "@/components/messages/utils";
import type { FolderCount, MessageCounts, MessageFolder } from "@/hooks/types";
import { applyMessageCountsDelta } from "@/hooks/utils";

function countsWith(
	overrides: Partial<Record<string, FolderCount>>,
): MessageCounts {
	const folders = {} as Record<MessageFolder, FolderCount>;
	for (const folder of [
		"inbox",
		"starred",
		"snoozed",
		"sent",
		"scheduled",
		"drafts",
		"archived",
		"spam",
		"trash",
	] as MessageFolder[]) {
		folders[folder] = { total: 0, unread: 0 };
	}
	for (const [folder, count] of Object.entries(overrides)) {
		if (folder in folders)
			folders[folder as MessageFolder] = count as FolderCount;
	}
	return { folders, customFolders: {}, mailboxes: [] };
}

describe("folder badges", () => {
	const counts = countsWith({
		inbox: { total: 40, unread: 7 },
		starred: { total: 9, unread: 4 },
		snoozed: { total: 3, unread: 2 },
		sent: { total: 120, unread: 0 },
		scheduled: { total: 2, unread: 0 },
		drafts: { total: 5, unread: 0 },
		archived: { total: 30, unread: 1 },
		spam: { total: 12, unread: 12 },
		trash: { total: 8, unread: 3 },
	});

	test("unread is what a reader has to act on", () => {
		for (const folder of [
			"inbox",
			"starred",
			"snoozed",
			"archived",
			"spam",
			"trash",
		] as MessageFolder[]) {
			expect(getFolderBadgeCount(folder, counts.folders)).toBe(
				counts.folders[folder].unread,
				folder,
			);
		}
	});

	test("drafts and scheduled show how much is waiting", () => {
		expect(getFolderBadgeCount("drafts", counts.folders)).toBe(5);
		expect(getFolderBadgeCount("scheduled", counts.folders)).toBe(2);
	});

	test("sent shows no badge at all", () => {
		expect(getFolderBadgeCount("sent", counts.folders)).toBeUndefined();
	});
});

describe("optimistic unread deltas", () => {
	const counts = () => {
		const next = countsWith({
			inbox: { total: 40, unread: 7 },
			spam: { total: 12, unread: 12 },
		});
		next.customFolders = { "folder-1": { total: 3, unread: 2 } };
		return next;
	};

	test("reading in another folder leaves the inbox alone", () => {
		const next = applyMessageCountsDelta(counts(), {
			folder: "spam",
			unreadDelta: -1,
		});
		expect(next.folders.inbox.unread).toBe(7);
		expect(next.folders.spam.unread).toBe(11);
	});

	test("reading in the inbox moves the inbox badge", () => {
		const next = applyMessageCountsDelta(counts(), {
			folder: "inbox",
			unreadDelta: -1,
		});
		expect(next.folders.inbox.unread).toBe(6);
		expect(next.folders.spam.unread).toBe(12);
	});

	test("a custom folder keeps its own unread count", () => {
		const next = applyMessageCountsDelta(counts(), {
			folder: "folder-1",
			unreadDelta: -1,
		});
		expect(next.customFolders["folder-1"].unread).toBe(1);
		expect(next.folders.inbox.unread).toBe(7);
	});

	test("a badge never goes below zero", () => {
		const next = applyMessageCountsDelta(counts(), {
			folder: "inbox",
			unreadDelta: -9,
		});
		expect(next.folders.inbox.unread).toBe(0);
	});

	test("a folder the client does not know is left alone", () => {
		const before = counts();
		const next = applyMessageCountsDelta(before, {
			folder: "not-a-folder",
			unreadDelta: 1,
		});
		expect(next).toEqual(before);
	});
});

describe("row state labels", () => {
	test("a delivery that has not finished says so", () => {
		expect(getSendStateKey("queued")).toBe("list.sendQueued");
		expect(getSendStateKey("sending")).toBe("list.sendSending");
		expect(getSendStateKey("failed")).toBe("list.sendFailed");
	});

	test("a delivered message needs no label", () => {
		expect(getSendStateKey("sent")).toBeNull();
		expect(getSendStateKey(null)).toBeNull();
		expect(getSendStateKey(undefined)).toBeNull();
	});
});

describe("the page range", () => {
	test("counts from the page index and the page size", () => {
		expect(getPageRange(0, 25, 25, 60)).toEqual({
			start: 1,
			end: 25,
			total: 60,
		});
		expect(getPageRange(1, 25, 25, 60)).toEqual({
			start: 26,
			end: 50,
			total: 60,
		});
		expect(getPageRange(2, 25, 10, 60)).toEqual({
			start: 51,
			end: 60,
			total: 60,
		});
	});

	test("an empty folder has no range", () => {
		expect(getPageRange(0, 25, 0, 0)).toEqual({ start: 0, end: 0, total: 0 });
	});
});
