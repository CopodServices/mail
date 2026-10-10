import { loadDraft } from "@/lib/mail-api/client";
import type { ComposeDraft } from "./types";

export async function fetchDraft(draftId: string): Promise<ComposeDraft> {
	const draft = await loadDraft(draftId);
	return {
		id: draft.id,
		mailboxId: draft.mailbox_id,
		fromAddr: draft.from,
		toAddr: draft.to,
		ccAddr: draft.cc,
		bccAddr: draft.bcc,
		subject: draft.subject || null,
		textBody: draft.text || null,
		htmlBody: draft.html || null,
		inReplyTo: draft.in_reply_to,
		references: draft.references_header,
		threadId: draft.thread_id,
		attachments: (draft.attachments ?? []).map((attachment) => ({
			id: String(attachment.index),
			filename: attachment.filename ?? "attachment",
			size: attachment.size_bytes,
			type: attachment.content_type ?? "application/octet-stream",
			disposition: "attachment" as const,
		})),
	};
}

export function formatAttachmentSize(size: number): string {
	if (size < 1024) return `${size} B`;
	if (size < 1024 * 1024) return `${Math.ceil(size / 1024)} KB`;
	return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}
