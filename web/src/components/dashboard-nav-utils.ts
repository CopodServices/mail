import type { MessageCounts, MessageFolder } from "@/hooks/types";
import { bulkMessageAction } from "@/lib/api/endpoints";
import type { BulkMessageAction } from "@/lib/api/types";

/**
 * What a folder's badge counts. Unread mail is what a reader has to act on, so
 * every folder that can hold unread mail shows its unread count. Drafts and
 * Scheduled are lists of unfinished work, so they show how much is waiting, and
 * Sent shows nothing: mail the sender owns is never unread.
 */
export function getFolderBadgeCount(
	folder: MessageFolder,
	counts: MessageCounts["folders"],
): number | undefined {
	if (folder === "sent") return undefined;
	const entry = counts[folder];
	if (!entry) return undefined;
	return folder === "drafts" || folder === "scheduled"
		? entry.total
		: entry.unread;
}

async function moveMessages(payload: {
	messageIds: string[];
	action: BulkMessageAction;
	folderId?: string;
}) {
	await bulkMessageAction(
		payload.messageIds,
		payload.action,
		payload.folderId ?? null,
	).catch(() => {
		throw new Error("Unable to move messages");
	});
	window.dispatchEvent(new Event("copod:messages-changed"));
}

export function moveMessagesToSystemFolder(
	messageIds: string[],
	action: "archive" | "spam" | "trash",
) {
	return moveMessages({ messageIds, action });
}

export function moveMessagesToCustomFolder(
	messageIds: string[],
	folderId: string,
) {
	return moveMessages({ messageIds, action: "folder", folderId });
}
