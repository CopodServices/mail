import type {
	FolderCount,
	MailboxCount,
	Message,
	MessageCounts,
	MessageFolder,
	MessageListResponse,
} from "@/hooks/types";

/**
 * The mail API is the source of truth. These adapters translate its responses
 * (snake_case fields, data/meta envelopes) into the shapes the client renders,
 * so the UI stays unchanged while the boundary matches the API.
 */

type ApiMessage = {
	id: string;
	mailbox_id: string | null;
	direction: "inbound" | "outbound";
	status: string;
	envelope_from: string;
	envelope_to: string;
	message_id: string | null;
	subject: string | null;
	header_from: string | null;
	size_bytes: number;
	received_at: string;
	read: boolean;
	starred: boolean;
	folder: string;
	snoozed_until?: string | null;
	scheduled_at?: string | null;
	send_state?: string | null;
	send_error?: string | null;
};

type ApiMessageList = {
	data?: ApiMessage[];
	meta?: { total?: number; limit?: number; next_cursor?: string | null };
};

type ApiCounts = {
	data?: {
		folders?: Record<string, FolderCount>;
		custom_folders?: Record<string, FolderCount>;
		mailboxes?: Array<{ mailbox_id: string; total: number; unread: number }>;
	};
};

type ApiAttachment = {
	index: number;
	filename: string | null;
	content_type: string | null;
	content_id?: string | null;
	size_bytes: number;
};

type ApiMessageDetail = {
	data?: {
		message?: ApiMessage;
		text?: string | null;
		html?: string | null;
		attachments?: ApiAttachment[];
		unsubscribe_url?: string | null;
	};
};

function toMessage(dto: ApiMessage): Message {
	const folder = dto.folder as MessageFolder;
	return {
		id: dto.id,
		userId: dto.mailbox_id ?? "",
		mailboxId: dto.mailbox_id,
		folderId: isSystemFolder(folder) ? null : folder,
		direction: dto.direction,
		providerMessageId: dto.message_id,
		fromAddr: dto.header_from ?? dto.envelope_from,
		toAddr: dto.envelope_to,
		subject: dto.subject,
		snippet: null,
		status: dto.status,
		folder,
		// A copy in the sender's own Sent folder is read by definition.
		read: dto.read || dto.direction === "outbound",
		starred: dto.starred,
		snoozedUntil: dto.snoozed_until ?? null,
		scheduledAt: dto.scheduled_at ?? null,
		sendState: (dto.send_state as Message["sendState"]) ?? null,
		sendError: dto.send_error ?? null,
		threadId: dto.message_id,
		createdAt: dto.received_at,
		threadCount: 1,
		threadUnread: dto.read || dto.direction === "outbound" ? 0 : 1,
	};
}

export function adaptMessageList(payload: ApiMessageList): MessageListResponse {
	return {
		messages: (payload.data ?? []).map(toMessage),
		total: payload.meta?.total ?? payload.data?.length ?? 0,
		limit: payload.meta?.limit ?? payload.data?.length ?? 0,
		nextCursor: payload.meta?.next_cursor ?? null,
	};
}

/** The folders every mailbox has, which are not custom folder ids. */
export const SYSTEM_FOLDERS: MessageFolder[] = [
	"inbox",
	"starred",
	"snoozed",
	"sent",
	"scheduled",
	"drafts",
	"archived",
	"trash",
	"spam",
];

export function isSystemFolder(folder: string): folder is MessageFolder {
	return (SYSTEM_FOLDERS as string[]).includes(folder);
}

export function adaptCounts(payload: ApiCounts): MessageCounts {
	const folders = {} as Record<MessageFolder, FolderCount>;
	for (const folder of SYSTEM_FOLDERS) {
		folders[folder] = { total: 0, unread: 0 };
	}
	for (const [folder, count] of Object.entries(payload.data?.folders ?? {})) {
		folders[folder as MessageFolder] = count;
	}
	const mailboxes: MailboxCount[] = (payload.data?.mailboxes ?? []).map(
		(mailbox) => ({
			mailboxId: mailbox.mailbox_id,
			total: mailbox.total,
			unread: mailbox.unread,
			inbox: 0,
		}),
	);
	return {
		folders,
		customFolders: payload.data?.custom_folders ?? {},
		mailboxes,
	};
}

/**
 * One MIME part as the client model. A part a body embeds by cid: carries a
 * Content-ID, which the reader uses to rewrite that reference to this part.
 */
export function toMessageAttachment(
	attachment: ApiAttachment,
	messageId: string,
) {
	const contentId = attachment.content_id?.trim() || null;
	return {
		contentId,
		disposition: contentId ? ("inline" as const) : ("attachment" as const),
		filename: attachment.filename ?? "attachment",
		// A stored message's parts are addressed by position in the message.
		id: String(attachment.index),
		messageId,
		size: attachment.size_bytes,
		type: attachment.content_type ?? "application/octet-stream",
	};
}

export function adaptMessageDetail(payload: ApiMessageDetail) {
	const message = payload.data?.message;
	if (!message) return { message: undefined };
	const attachments = (payload.data?.attachments ?? []).map((attachment) =>
		toMessageAttachment(attachment, message.id),
	);
	return {
		message: toMessage(message),
		body: {
			textBody: payload.data?.text ?? null,
			htmlBody: payload.data?.html ?? null,
		},
		attachments,
		unsubscribeUrl: payload.data?.unsubscribe_url ?? null,
	};
}

export type ApiThreadMessage = ApiMessage & {
	text?: string | null;
	html?: string | null;
	attachments?: ApiAttachment[];
};

/** A conversation member: the message plus its body and attachments. */
export function adaptThreadMessage(dto: ApiThreadMessage) {
	return {
		...toMessage(dto),
		textBody: dto.text ?? null,
		htmlBody: dto.html ?? null,
		attachments: (dto.attachments ?? []).map((attachment) =>
			toMessageAttachment(attachment, dto.id),
		),
	};
}
