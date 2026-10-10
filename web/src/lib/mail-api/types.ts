/**
 * Client-side mirror of the mail API request/response types the UI shares
 * across components. The API itself lives in the mail service; these are the
 * shapes its /api endpoints return.
 */

export type BulkMessageAction =
	| "archive"
	| "trash"
	| "spam"
	| "read"
	| "unread"
	| "inbox"
	| "folder"
	| "delete";

export type BulkMessagePayload = {
	messageIds?: string[];
	action?: BulkMessageAction;
	folderId?: string;
};

/** Folders whose messages can be permanently deleted or emptied in one go. */
export type PermanentDeleteFolder = "trash" | "spam";

export type EmptyFolderPayload = {
	mailboxId?: string;
	folder?: PermanentDeleteFolder;
};

export type EmptyFolderResponse = {
	ok?: boolean;
	deleted?: number;
	remaining?: number;
	error?: string;
};

export type MessageNavigationEntry = {
	id: string;
	unread: boolean;
};

export type MessageNavigationResponse = {
	messages: MessageNavigationEntry[];
};

export type TrashRetentionSettingsResponse = {
	days?: number | null;
	error?: unknown;
};
