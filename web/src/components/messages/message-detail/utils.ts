import { splitQuotedHtml } from "@/components/compose/rich-text-utils";
import type { Message } from "@/hooks/types";
import { apiFetch } from "@/lib/api/client";
import { getDisplayNameForAddress } from "@/lib/contacts/utils";
import {
	getEmailAddress,
	getEmailAddressList,
	normalizeEmailAddress,
} from "@/lib/email/address";
import { resolveEmailCidUrls } from "@/lib/email/html";
import { formatRecipientAddressList } from "@/lib/email/recipient-display";
import {
	htmlToReadableText,
	splitRepliedEmailContent,
} from "@/lib/email/reply-content-utils";
import {
	fetchCachedMessageDetail,
	getCachedMessageDetail,
} from "@/lib/messages/detail-cache";
import type {
	MessageAttachment,
	MessageBodyDisplay,
	MessageDetailResponse,
} from "./types";

export async function fetchMessageDetail(
	messageId: string,
): Promise<MessageDetailResponse> {
	return (await fetchCachedMessageDetail(messageId)) as MessageDetailResponse;
}

export function getCachedMessageDetailForDisplay(
	messageId: string,
): MessageDetailResponse | undefined {
	return getCachedMessageDetail(messageId) as MessageDetailResponse | undefined;
}

export async function fetchMessageMetadata(
	messageId: string,
): Promise<Pick<MessageDetailResponse, "attachments" | "unsubscribeUrl">> {
	const response = await apiFetch(`/api/messages/${messageId}/metadata`);
	const payload = (await response.json()) as {
		data?: {
			attachments?: Array<{
				index: number;
				filename: string | null;
				content_type: string | null;
				size_bytes: number;
			}>;
			unsubscribe_url?: string | null;
		};
	};
	return {
		attachments: (payload.data?.attachments ?? []).map((attachment) => ({
			contentId: null,
			disposition: "attachment" as const,
			filename: attachment.filename ?? "attachment",
			id: String(attachment.index),
			messageId,
			size: attachment.size_bytes,
			type: attachment.content_type ?? "application/octet-stream",
		})),
		unsubscribeUrl: payload.data?.unsubscribe_url ?? null,
	};
}

export function getMessageHeaderParties(
	message: Message,
	currentAccountName?: string,
) {
	return {
		fromName:
			message.direction === "outbound" && currentAccountName
				? currentAccountName
				: getDisplayNameForAddress(message.fromAddr, message.fromContactName),
		fromAddress: getEmailAddress(message.fromAddr),
		toName:
			message.direction === "inbound"
				? formatRecipientAddressList(message.toAddr, "address") || "—"
				: getDisplayNameForAddress(message.toAddr, message.toContactName),
	};
}

/**
 * The mailbox address this message reached, used as the reply sender and to
 * tell "sent" from "received" in quoted history. With several recipients the
 * mailbox's own address is whichever of them it can send as.
 */
export function getOwnAddressForMessage(
	message: Message,
	ownAddresses: string[],
): string {
	if (message.direction === "outbound")
		return getEmailAddress(message.fromAddr);
	const own = new Set(
		ownAddresses.map((address) => normalizeEmailAddress(address)),
	);
	const listed = [
		...getEmailAddressList(message.toAddr),
		...getEmailAddressList(message.ccAddr),
	];
	return (
		listed.find((address) => own.has(address)) ??
		ownAddresses[0] ??
		getEmailAddress(message.toAddr)
	);
}

export function getMessageBodyDisplay(
	textBody: string | null | undefined,
	htmlBody: string | null | undefined,
	fallback: string | null | undefined,
	ownAddress?: string,
): MessageBodyDisplay {
	// A message composed in Mailflare marks its quoted part, so the rich body can
	// stay rich and the quote fold on its own. Other mail falls back to the
	// text heuristics, which can only render the result as text.
	const marked = splitQuotedHtml(htmlBody);
	if (marked.quoted !== null) {
		const parts = splitRepliedEmailContent(htmlToReadableText(marked.body), {
			ownAddress,
		});
		return {
			latestContent: parts.latestContent,
			quotedContent: [],
			htmlBody: marked.body,
			quotedHtml: marked.quoted,
			hasQuotedContent: true,
		};
	}

	const textSource =
		textBody ?? (htmlToReadableText(htmlBody) || fallback || "");
	const parts = splitRepliedEmailContent(textSource, { ownAddress });

	return {
		...parts,
		// External HTML quotes are part of the sender's layout. Rendering the
		// generated text instead adds line breaks between their HTML blocks.
		quotedContent: htmlBody ? [] : parts.quotedContent,
		htmlBody: htmlBody ?? null,
		quotedHtml: null,
		hasQuotedContent: !htmlBody && parts.quotedContent.length > 0,
	};
}

export function getAttachmentUrl(
	messageId: string,
	attachmentId: string,
): string {
	return `/api/messages/${messageId}/attachments/${attachmentId}?preview=1`;
}

export function resolveInlineAttachmentUrls(
	htmlBody: string | null,
	messageId: string,
	attachments: MessageAttachment[],
): string | null {
	if (!htmlBody) return null;

	const urls = new Map<string, string>();
	for (const attachment of attachments) {
		if (attachment.contentId)
			urls.set(
				attachment.contentId.replace(/^<|>$/g, ""),
				getAttachmentUrl(messageId, attachment.id),
			);
	}
	return resolveEmailCidUrls(htmlBody, urls);
}

export function formatAttachmentSize(size: number): string {
	if (size < 1024) return `${size} B`;
	if (size < 1024 * 1024) return `${Math.ceil(size / 1024)} KB`;
	return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}
