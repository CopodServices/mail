import { useEffect, useMemo, useState } from "react";
import type { Message, MessageFilterOptions, MessageFolder } from "./types";
import {
	clearMessageCountsCache,
	clearMessageListCache,
	fetchMessageList,
	getMessageQueryParams,
} from "./utils";

const REFRESH_DEBOUNCE_MS = 150;

/**
 * One folder's messages, paged with the API's keyset cursor. The hook keeps the
 * cursors of the pages already visited, so Previous walks back through what was
 * read and Next follows the cursor the last page returned.
 */
export function useMessages(
	folder: MessageFolder,
	mailboxId?: string | null,
	filters?: MessageFilterOptions,
	enabled = true,
	folderId?: string | null,
) {
	const [messages, setMessages] = useState<Message[]>([]);
	const [isLoading, setIsLoading] = useState(true);
	const [total, setTotal] = useState(0);
	const [limit, setLimit] = useState(filters?.limit ?? 25);
	// The cursor each visited page was opened with; the first page has none.
	const [cursors, setCursors] = useState<Array<string | null>>([null]);
	const [nextCursor, setNextCursor] = useState<string | null>(null);

	// Anything but the cursor identifies the list: a change to it starts over.
	const listKey = useMemo(
		() =>
			getMessageQueryParams(
				folder,
				mailboxId,
				{ ...filters, cursor: null },
				folderId,
			).toString(),
		[folder, mailboxId, folderId, filters],
	);
	const cursor = cursors[cursors.length - 1] ?? null;

	useEffect(() => {
		setCursors([null]);
		setNextCursor(null);
	}, [listKey]);

	useEffect(() => {
		if (!enabled) return;
		let cancelled = false;
		let refreshTimer: number | null = null;
		let requestNumber = 0;
		async function loadMessages(force = false, showLoading = false) {
			const currentRequest = ++requestNumber;
			if (showLoading) setIsLoading(true);
			try {
				const params = getMessageQueryParams(
					folder,
					mailboxId,
					{ ...filters, cursor },
					folderId,
				);
				const data = await fetchMessageList(params, force);
				if (!cancelled && currentRequest === requestNumber) {
					setMessages(data.messages ?? []);
					setTotal(data.total ?? 0);
					setLimit(data.limit ?? filters?.limit ?? 25);
					setNextCursor(data.nextCursor ?? null);
				}
			} finally {
				if (!cancelled && currentRequest === requestNumber) setIsLoading(false);
			}
		}

		void loadMessages(false, true);
		function onMessagesChanged() {
			clearMessageListCache();
			clearMessageCountsCache();
			if (document.visibilityState !== "visible") return;
			if (refreshTimer) window.clearTimeout(refreshTimer);
			refreshTimer = window.setTimeout(() => {
				refreshTimer = null;
				void loadMessages(true);
			}, REFRESH_DEBOUNCE_MS);
		}
		window.addEventListener("copod:messages-changed", onMessagesChanged);

		return () => {
			cancelled = true;
			window.removeEventListener("copod:messages-changed", onMessagesChanged);
			if (refreshTimer) window.clearTimeout(refreshTimer);
		};
	}, [enabled, listKey, cursor]);

	const unreadCount = messages.filter(
		(message) => message.direction === "inbound" && !message.read,
	).length;

	return {
		messages,
		unreadCount,
		isLoading,
		total,
		limit,
		/** Zero-based index of the page on screen, for the range label. */
		pageIndex: cursors.length - 1,
		hasNext: nextCursor !== null,
		hasPrevious: cursors.length > 1,
		nextPage: () => {
			if (nextCursor) setCursors((current) => [...current, nextCursor]);
		},
		previousPage: () => {
			setCursors((current) =>
				current.length > 1 ? current.slice(0, -1) : current,
			);
		},
		updateMessages: setMessages,
	};
}
