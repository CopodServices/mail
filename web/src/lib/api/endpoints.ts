import { apiFetch } from "@/lib/api/client";
import type { BulkMessageAction, MailboxStorage } from "./types";

/**
 * Typed helpers for the mail API's write endpoints, built on the transport in
 * ./client.ts (cookies, CSRF, 401 handling) — the only place a request is sent.
 *
 * The API is the source of truth: requests use its vocabulary (snake_case
 * bodies, data/meta envelopes) and these helpers unwrap the envelope so the UI
 * keeps working with plain values.
 */

type Envelope<T> = { data?: T };

const JSON_HEADERS = { "Content-Type": "application/json" };

export async function setMessageRead(
	messageId: string,
	read: boolean,
): Promise<void> {
	const response = await apiFetch("/api/messages/" + messageId + "/read", {
		method: "POST",
		headers: JSON_HEADERS,
		body: JSON.stringify({ read }),
	});
	if (!response.ok) throw new Error("Unable to update message");
}

export async function setMessageStarred(
	messageId: string,
	starred: boolean,
): Promise<boolean> {
	const response = await apiFetch("/api/messages/" + messageId + "/star", {
		method: "POST",
		headers: JSON_HEADERS,
		body: JSON.stringify({ starred }),
	});
	if (!response.ok) throw new Error("Unable to update message");
	const body = (await response.json()) as Envelope<{ starred: boolean }>;
	return body.data?.starred ?? starred;
}

export async function bulkMessageAction(
	messageIds: string[],
	action: BulkMessageAction,
	folderId?: string | null,
): Promise<void> {
	const response = await apiFetch("/api/messages/bulk", {
		method: "POST",
		headers: JSON_HEADERS,
		body: JSON.stringify({
			message_ids: messageIds,
			action,
			folder: folderId ?? null,
		}),
	});
	if (!response.ok) throw new Error("Unable to update messages");
}

type SendMessageInput = {
	from: string;
	to: string[];
	cc?: string[];
	bcc?: string[];
	subject: string;
	text?: string;
	html?: string;
	/** Draft whose stored attachments go out with the message. */
	draftId?: string | null;
	/** Send no earlier than this time; the message waits in Scheduled until then. */
	scheduledAt?: string | null;
};

export async function sendMessage(input: SendMessageInput): Promise<{
	stored: number;
	queued: number;
	scheduled?: boolean;
}> {
	const response = await apiFetch("/api/send", {
		method: "POST",
		headers: JSON_HEADERS,
		body: JSON.stringify({
			from: input.from,
			to: input.to,
			cc: input.cc ?? [],
			bcc: input.bcc ?? [],
			subject: input.subject,
			text: input.text,
			html: input.html,
			draft_id: input.draftId ?? null,
			scheduled_at: input.scheduledAt ?? null,
		}),
	});
	if (!response.ok) throw new Error("Unable to send message");
	const body = (await response.json()) as Envelope<{
		stored: number;
		queued: number;
		scheduled?: boolean;
	}>;
	return body.data ?? { stored: 0, queued: 0, scheduled: false };
}

type ApiFolder = {
	id: string;
	name: string;
	kind: string;
	color?: string | null;
};

export async function fetchFolders(mailboxId: string): Promise<ApiFolder[]> {
	const params = new URLSearchParams({ mailbox_id: mailboxId });
	const response = await apiFetch("/api/folders?" + params.toString());
	if (!response.ok) throw new Error("Unable to load folders");
	const body = (await response.json()) as { data?: ApiFolder[] };
	return body.data ?? [];
}

export async function createFolder(
	mailboxId: string,
	name: string,
	color: string,
): Promise<ApiFolder> {
	const response = await apiFetch("/api/folders", {
		method: "POST",
		headers: JSON_HEADERS,
		body: JSON.stringify({ mailbox_id: mailboxId, name, color }),
	});
	if (!response.ok) throw new Error("Unable to create folder");
	const body = (await response.json()) as { data?: ApiFolder };
	if (!body.data) throw new Error("Unable to create folder");
	return body.data;
}

/**
 * Storage used and the quota for every mailbox the caller can see — one, for a
 * signed-in session (the API scopes the list to the session's own mailbox).
 */
export async function fetchMailboxStorage(): Promise<MailboxStorage[]> {
	const response = await apiFetch("/api/mailboxes");
	if (!response.ok) throw new Error("Unable to load mailbox storage");
	const body = (await response.json()) as {
		data?: Array<{ id: string; used_bytes?: number; quota_bytes?: number }>;
	};
	return (body.data ?? []).map((mailbox) => ({
		mailboxId: mailbox.id,
		usedBytes: mailbox.used_bytes ?? 0,
		quotaBytes: mailbox.quota_bytes ?? 0,
	}));
}

type ApiDraftAttachment = {
	index: number;
	filename: string | null;
	content_type: string | null;
	size_bytes: number;
};

type ApiDraft = {
	id: string;
	mailbox_id: string | null;
	from: string;
	to: string;
	cc: string;
	bcc: string;
	subject: string;
	text: string;
	html: string;
	in_reply_to: string | null;
	references_header: string | null;
	thread_id: string | null;
	attachments: ApiDraftAttachment[];
	updated_at: string;
};

export type DraftInput = {
	mailbox_id?: string | null;
	from: string;
	to?: string;
	cc?: string;
	bcc?: string;
	subject?: string;
	text?: string;
	html?: string;
	in_reply_to?: string | null;
	references_header?: string | null;
	thread_id?: string | null;
};

export async function saveDraft(
	input: DraftInput,
	draftId?: string | null,
): Promise<string> {
	const response = await apiFetch(
		draftId ? "/api/drafts/" + draftId : "/api/drafts",
		{
			method: draftId ? "PATCH" : "POST",
			headers: JSON_HEADERS,
			body: JSON.stringify(input),
		},
	);
	if (!response.ok) throw new Error("Unable to save draft");
	const body = (await response.json()) as { data?: { id?: string } };
	if (!body.data?.id) throw new Error("Unable to save draft");
	return body.data.id;
}

export async function loadDraft(id: string): Promise<ApiDraft> {
	const response = await apiFetch("/api/drafts/" + id);
	if (!response.ok) throw new Error("Unable to load draft");
	const body = (await response.json()) as { data?: ApiDraft };
	if (!body.data) throw new Error("Unable to load draft");
	return body.data;
}

export async function deleteDraft(id: string): Promise<void> {
	await apiFetch("/api/drafts/" + id, { method: "DELETE" });
}

export async function uploadDraftAttachments(
	id: string,
	files: File[],
): Promise<ApiDraftAttachment[]> {
	const form = new FormData();
	for (const file of files) form.append("attachments", file);
	const response = await apiFetch("/api/drafts/" + id + "/attachments", {
		method: "POST",
		body: form,
	});
	if (!response.ok) throw new Error("Unable to attach the files");
	const body = (await response.json()) as { data?: ApiDraftAttachment[] };
	return body.data ?? [];
}

export async function removeDraftAttachment(
	id: string,
	attachmentId: string,
): Promise<ApiDraftAttachment[]> {
	const response = await apiFetch(
		"/api/drafts/" + id + "/attachments/" + attachmentId,
		{ method: "DELETE" },
	);
	if (!response.ok) throw new Error("Unable to remove the attachment");
	const body = (await response.json()) as { data?: ApiDraftAttachment[] };
	return body.data ?? [];
}
