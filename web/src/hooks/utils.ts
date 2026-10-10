import { adaptCounts, adaptMessageList } from "@/lib/api/adapters";
import { apiFetch } from "@/lib/api/client";
import { setCachedMessageRead } from "@/lib/messages/detail-cache";
import { parseSearchQuery } from "@/lib/search/query-utils";
import type {
	MessageCounts,
	MessageFilterOptions,
	MessageFolder,
	MessageListResponse,
} from "./types";

/**
 * The search string goes to the server whole; operators are parsed there
 * against the full-text index. Only the read state is lifted out here so the
 * unread toggle and `is:unread` share one `read` parameter.
 */
function parseMessageSearchQuery(query: string): MessageFilterOptions {
	const parsed = parseSearchQuery(query);
	const filters: MessageFilterOptions = {};
	if (parsed.read) filters.read = parsed.read;
	const remaining = query
		.replace(/(^|\s)(is:(un)?read|:(un)?read)(?=\s|$)/gi, " ")
		.replace(/\s+/g, " ")
		.trim();
	if (remaining) filters.query = remaining;
	return filters;
}

export function getMessageQueryParams(
	folder: MessageFolder,
	mailboxId?: string | null,
	filters?: MessageFilterOptions,
	folderId?: string | null,
) {
	// The API filters by folder; custom folders are passed as their folder id.
	const params = new URLSearchParams();
	const apiFolder = folderId ?? folder;
	if (apiFolder) params.set("folder", apiFolder);
	if (mailboxId) params.set("mailbox_id", mailboxId);

	const searchFilters = filters?.query
		? parseMessageSearchQuery(filters.query)
		: null;
	const parsedFilters = searchFilters
		? {
				...filters,
				...searchFilters,
				read: filters?.read ?? searchFilters.read,
			}
		: filters;

	if (parsedFilters?.query?.trim()) params.set("q", parsedFilters.query.trim());
	if (parsedFilters?.read && parsedFilters.read !== "all")
		params.set("read", parsedFilters.read);
	if (filters?.limit) params.set("limit", String(filters.limit));
	if (filters?.offset) params.set("offset", String(filters.offset));

	return params;
}

const messageCountsCache = new Map<string, MessageCounts>();
const messageCountsRequests = new Map<
	string,
	Promise<MessageCounts | undefined>
>();
const messageListCache = new Map<string, MessageListResponse>();
const messageListRequests = new Map<string, Promise<MessageListResponse>>();
let messageCacheGeneration = 0;
let messageCountsGeneration = 0;

export function clearMessageCountsCache() {
	messageCountsGeneration += 1;
	messageCountsCache.clear();
	messageCountsRequests.clear();
}

export function clearMessageListCache() {
	messageCacheGeneration += 1;
	messageListCache.clear();
	messageListRequests.clear();
}

/**
 * Mirror a read/unread change into the cached lists and details, so a list
 * remounted later (or a detail reopened) never shows the old state.
 */
export function markMessagesReadInCaches(messageIds: string[], read: boolean) {
	const ids = new Set(messageIds);
	for (const [key, response] of messageListCache) {
		if (!response.messages?.some((message) => ids.has(message.id))) continue;
		messageListCache.set(key, {
			...response,
			messages: response.messages.map((message) => {
				if (!ids.has(message.id)) return message;
				const threadSize = message.threadMessageIds?.length;
				return {
					...message,
					read,
					...(threadSize !== undefined
						? { threadUnread: read ? 0 : threadSize }
						: {}),
				};
			}),
		});
	}
	for (const id of ids) setCachedMessageRead(id, read);
}

// Caches must drop on every change, including ones made while no list is mounted
// (e.g. reading a message from the popup on a page without the inbox list).
if (typeof window !== "undefined") {
	window.addEventListener("copod:messages-changed", () => {
		clearMessageListCache();
		clearMessageCountsCache();
	});
}

export async function fetchMessageCounts(
	mailboxId?: string | null,
	force = false,
): Promise<MessageCounts | undefined> {
	const key = mailboxId ?? "all";
	if (!force && messageCountsCache.has(key)) return messageCountsCache.get(key);
	if (!force && messageCountsRequests.has(key))
		return messageCountsRequests.get(key);

	const requestGeneration = messageCacheGeneration;
	const countsGeneration = messageCountsGeneration;
	const request = (async () => {
		const params = new URLSearchParams();
		if (mailboxId) params.set("mailbox_id", mailboxId);
		const query = params.toString();
		const res = await apiFetch(
			`/api/messages/counts${query ? `?${query}` : ""}`,
		);
		const counts = adaptCounts(await res.json());
		if (
			counts &&
			requestGeneration === messageCacheGeneration &&
			countsGeneration === messageCountsGeneration
		) {
			messageCountsCache.set(key, counts);
		}
		return counts;
	})().finally(() => {
		if (messageCountsRequests.get(key) === request) {
			messageCountsRequests.delete(key);
		}
	});

	messageCountsRequests.set(key, request);
	return request;
}

export async function fetchMessageList(
	params: URLSearchParams,
	force = false,
): Promise<MessageListResponse> {
	const key = params.toString();
	if (!force && messageListCache.has(key))
		return messageListCache.get(key) ?? {};
	if (!force && messageListRequests.has(key))
		return messageListRequests.get(key) ?? {};

	const requestGeneration = messageCacheGeneration;
	const request = apiFetch(`/api/messages?${key}`)
		.then((res) => res.json())
		.then((data) => {
			const response = adaptMessageList(data);
			if (requestGeneration === messageCacheGeneration) {
				messageListCache.set(key, response);
			}
			return response;
		})
		.finally(() => {
			if (requestGeneration === messageCacheGeneration) {
				messageListRequests.delete(key);
			}
		});

	messageListRequests.set(key, request);
	return request;
}
