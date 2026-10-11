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

/** Folders whose messages can be permanently deleted or emptied in one go. */
export type PermanentDeleteFolder = "trash" | "spam";

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

/** The mailbox behind a signed-in webmail session. */
export type SessionMailbox = {
	mailbox_id: string;
	address: string;
	domain: string;
};

/** One mailbox's storage meter, from GET /api/mailboxes. */
export type MailboxStorage = {
	mailboxId: string;
	usedBytes: number;
	quotaBytes: number;
};

/** How another mail app connects to this mailbox, from GET /api/settings/clients. */
export type ClientConnection = {
	smtpHost: string;
	smtpPort: number;
	smtpSecurity: "starttls" | "none";
	username: string | null;
	imapSupported: boolean;
	imapHost: string;
	imapPort: number;
	imapSecurity: "tls" | "none";
	pop3Supported: boolean;
	pop3Host: string;
	pop3Port: number;
	pop3Security: "tls" | "none";
};
