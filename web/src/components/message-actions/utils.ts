import {
	ArchiveIcon,
	InboxIcon,
	ShieldAlertIcon,
	Trash2Icon,
} from "lucide-react";
import {
	escapeHtml,
	htmlToPlainText,
	textToHtml,
	wrapQuotedHtml,
} from "@/components/compose/rich-text-utils";
import { sanitizeEmailHtml } from "@/components/messages/message-detail/email-html-sanitizer";
import { markMessagesReadInCaches } from "@/hooks/utils";
import { apiFetch } from "@/lib/api/client";
import { bulkMessageAction } from "@/lib/api/endpoints";
import type { BulkMessageAction } from "@/lib/api/types";
import {
	getEmailAddress,
	normalizeEmailAddress,
	splitEmailAddressList,
} from "@/lib/email/address";
import { formatUserDate } from "@/lib/time/utils";
import type {
	ForwardDraftInput,
	MoveMessageActionItem,
	ReplyableMessage,
	ReplyDraftInput,
	ReplyMode,
	ReplyRecipients,
} from "./types";

export function getMessageBackHref(
	direction: "inbound" | "outbound",
	status: string,
) {
	if (status === "trash") return "/trash";
	if (status === "spam") return "/spam";
	if (status === "archived") return "/archived";
	if (status === "draft") return "/drafts";
	return direction === "inbound" ? "/inbox" : "/sent";
}

export async function runSingleMessageAction(
	messageId: string,
	action: BulkMessageAction,
) {
	await bulkMessageAction([messageId], action).catch(() => {
		throw new Error("Unable to update message");
	});

	if (action === "read" || action === "unread")
		markMessagesReadInCaches([messageId], action === "read");
	window.dispatchEvent(new Event("copod:messages-changed"));
}

export function openUnsubscribeUrl(url: string) {
	window.open(url, "_blank", "noopener,noreferrer");
}

export function confirmTrashWithoutUnsubscribe() {
	return window.confirm(
		"This email does not provide an unsubscribe link. Move it to Trash?",
	);
}

export function getMoveMessageActions(
	status: string,
	direction: "inbound" | "outbound",
): MoveMessageActionItem[] {
	const actions: MoveMessageActionItem[] = [];
	if ((status === "archived" || status === "spam") && direction === "inbound") {
		actions.push({
			action: "inbox",
			labelKey: status === "spam" ? "common.notSpam" : "navigation.inbox",
			icon: InboxIcon,
		});
	}
	if (status !== "archived")
		actions.push({
			action: "archive",
			labelKey: "navigation.archived",
			icon: ArchiveIcon,
		});
	if (status !== "spam")
		actions.push({
			action: "spam",
			labelKey: "navigation.spam",
			icon: ShieldAlertIcon,
		});
	if (status !== "trash")
		actions.push({
			action: "trash",
			labelKey: "navigation.trash",
			icon: Trash2Icon,
		});
	return actions;
}

export function getMessageActionRedirect(action: BulkMessageAction) {
	if (action === "trash") return "/trash";
	if (action === "spam") return "/spam";
	if (action === "archive") return "/archived";
	if (action === "inbox") return "/inbox";
	return null;
}

function buildReplySubject(subject: string | null | undefined) {
	const trimmed = (subject ?? "").trim();
	if (!trimmed) return "Re:";
	return /^re:/i.test(trimmed) ? trimmed : `Re: ${trimmed}`;
}

const QUOTE_STYLE =
	"margin:0 0 0 0;border-left:1px solid #ccc;padding-left:1ex;opacity:0.6";

/**
 * The folded quote under a reply: the "On <date>, <sender> wrote:" line every
 * client recognises, then the original as HTML (sanitised) or as text.
 */
function buildReplyQuoteHtml(
	senderAddress: string,
	sentAt: string | null | undefined,
	bodyText: string | null | undefined,
	bodyHtml: string | null | undefined,
) {
	const original = bodyHtml
		? sanitizeEmailHtml(bodyHtml, { forOutgoing: true })
		: textToHtml(bodyText);
	if (!original) return null;
	const when = sentAt
		? formatUserDate(sentAt, {
				weekday: "short",
				month: "short",
				day: "numeric",
				year: "numeric",
				hour: "numeric",
				minute: "2-digit",
			})
		: "an earlier date";
	return wrapQuotedHtml(
		`<div style="margin-top:1.4em">On ${escapeHtml(when)}, ${escapeHtml(senderAddress)} wrote:</div><blockquote style="${QUOTE_STYLE}">${original}</blockquote>`,
	);
}

/**
 * Who a reply goes to. A plain reply answers the sender; reply-all also keeps
 * everyone else on the To and Cc lines, minus the mailbox's own addresses so
 * the author is not mailing themselves. Replying to a sent message re-addresses
 * its original recipients.
 */
export function getReplyRecipients(
	message: ReplyableMessage,
	ownAddresses: string[],
	mode: ReplyMode,
): ReplyRecipients {
	const own = new Set(
		ownAddresses.map((address) => normalizeEmailAddress(address)),
	);
	const seen = new Set<string>();
	const unique = (entries: string[]) =>
		entries.filter((entry) => {
			const key = normalizeEmailAddress(entry);
			if (!key || own.has(key) || seen.has(key)) return false;
			seen.add(key);
			return true;
		});

	if (message.direction === "outbound") {
		const to = unique(splitEmailAddressList(message.toAddr));
		const cc =
			mode === "replyAll" ? unique(splitEmailAddressList(message.ccAddr)) : [];
		return { to, cc };
	}

	const to = unique([message.fromAddr]);
	if (mode !== "replyAll") return { to, cc: [] };
	const cc = unique([
		...splitEmailAddressList(message.toAddr),
		...splitEmailAddressList(message.ccAddr),
	]);
	return { to, cc };
}

/** Reply-all is only worth offering when it would reach someone a plain reply would not. */
export function hasAdditionalRecipients(
	message: ReplyableMessage,
	ownAddresses: string[],
): boolean {
	const all = getReplyRecipients(message, ownAddresses, "replyAll");
	const single = getReplyRecipients(message, ownAddresses, "reply");
	return all.to.length + all.cc.length > single.to.length + single.cc.length;
}

/** The threading headers a reply must carry so both sides file it in the same conversation. */
export function getReplyThreading(message: ReplyableMessage) {
	const parentId =
		(message.providerMessageId ?? "").trim().replace(/^<|>$/g, "") || null;
	const chain = (message.references ?? "")
		.split(/\s+/)
		.map((id) => id.replace(/^<|>$/g, ""))
		.filter(Boolean);
	if (parentId && !chain.includes(parentId)) chain.push(parentId);
	return {
		inReplyTo: parentId,
		references: chain.length ? chain.join(" ") : null,
		threadId: message.threadId ?? parentId,
	};
}

function buildForwardSubject(subject: string | null | undefined) {
	const trimmed = (subject ?? "").trim();
	if (!trimmed) return "Fwd:";
	return /^fwd?:/i.test(trimmed) ? trimmed : `Fwd: ${trimmed}`;
}

/** The header block mail clients put above a forwarded message, as folded HTML. */
function buildForwardHtml(
	message: ForwardDraftInput["message"],
	bodyText: string | null | undefined,
	bodyHtml: string | null | undefined,
) {
	const lines = [
		`From: ${message.fromAddr}`,
		`Date: ${formatUserDate(message.createdAt, { weekday: "short", month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit" })}`,
		`Subject: ${message.subject ?? "(no subject)"}`,
		`To: ${message.toAddr}`,
	];
	if (message.ccAddr) lines.push(`Cc: ${message.ccAddr}`);
	const header = `<div>---------- Forwarded message ---------<br>${lines.map(escapeHtml).join("<br>")}</div><br>`;
	const original = bodyHtml
		? sanitizeEmailHtml(bodyHtml, { forOutgoing: true })
		: textToHtml(bodyText);
	return wrapQuotedHtml(`${header}${original ?? ""}`);
}

/**
 * A forward keeps the original's attachments (copied onto the draft server-side)
 * and references its Message-ID so recipients who already have it see the link.
 */
export async function createForwardDraft({
	mailboxId,
	ownAddress,
	message,
	bodyText,
	bodyHtml,
}: ForwardDraftInput) {
	const html = buildForwardHtml(message, bodyText, bodyHtml);
	const response = await apiFetch("/api/drafts", {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify({
			mailbox_id: mailboxId ?? null,
			from: getEmailAddress(ownAddress ?? ""),
			to: "",
			subject: buildForwardSubject(message.subject),
			html,
			text: htmlToPlainText(html),
			references_header: getReplyThreading(message).references,
			thread_id: message.threadId,
			forward_of_message_id: message.id,
		}),
	});
	const data = (await response.json()) as {
		data?: { id: string };
		error?: string;
	};
	if (!response.ok || !data.data)
		throw new Error(data.error ?? "Unable to start forward");
	return data.data.id;
}

export async function createReplyDraft({
	mailboxId,
	senderAddress,
	ownAddress,
	subject,
	bodyText,
	bodyHtml,
	sentAt,
	recipients,
	threading,
}: ReplyDraftInput) {
	if (recipients.to.length === 0) throw new Error("Sender address is required");
	const html =
		buildReplyQuoteHtml(senderAddress, sentAt, bodyText, bodyHtml) ?? "";

	const response = await apiFetch("/api/drafts", {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify({
			mailbox_id: mailboxId ?? null,
			// The API rejects the draft unless `from` matches the mailbox address.
			from: getEmailAddress(ownAddress ?? ""),
			to: recipients.to.join(", "),
			cc: recipients.cc.join(", "),
			subject: buildReplySubject(subject),
			html,
			text: htmlToPlainText(html),
			in_reply_to: threading?.inReplyTo ?? null,
			references_header: threading?.references ?? null,
			thread_id: threading?.threadId ?? null,
		}),
	});
	const data = (await response.json()) as {
		data?: { id: string };
		error?: string;
	};
	if (!response.ok || !data.data)
		throw new Error(data.error ?? "Unable to create reply draft");
	return data.data.id;
}
