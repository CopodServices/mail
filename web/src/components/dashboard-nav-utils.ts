import type { MessageCounts, MessageFolder } from "@/hooks/types";
import { bulkMessageAction } from "@/lib/mail-api/client";
import type { BulkMessageAction } from "@/lib/mail-api/types";

export function getFolderNavCount(
	folder: MessageFolder,
	counts: MessageCounts["folders"],
): number | undefined {
	return counts[folder].unread;
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
