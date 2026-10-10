import { useEffect, useState } from "react";
import type { ThreadMessage } from "@/hooks/types";
import { apiFetch } from "@/lib/api/client";
import {
	type ApiThreadMessage,
	adaptThreadMessage,
} from "@/lib/mail-api/adapters";
import type { UseMessageThreadResult } from "./conversation-thread-types";

/** Loads every message in the conversation of `messageId`; refetches when mail changes. */
export function useMessageThread(
	messageId: string,
	threadId: string | null | undefined,
): UseMessageThreadResult {
	const [messages, setMessages] = useState<ThreadMessage[]>([]);
	const [loading, setLoading] = useState(false);

	useEffect(() => {
		if (!threadId) return;
		let cancelled = false;
		let refreshTimer: number | null = null;
		async function load() {
			setLoading(true);
			try {
				const response = await apiFetch(`/api/messages/${messageId}/thread`);
				const payload = (await response.json()) as {
					data?: { messages?: ApiThreadMessage[] };
				};
				if (!cancelled)
					setMessages(
						response.ok
							? (payload.data?.messages ?? []).map(adaptThreadMessage)
							: [],
					);
			} catch {
				if (!cancelled) setMessages([]);
			} finally {
				if (!cancelled) setLoading(false);
			}
		}
		void load();
		function scheduleLoad() {
			if (document.visibilityState !== "visible") return;
			if (refreshTimer) window.clearTimeout(refreshTimer);
			refreshTimer = window.setTimeout(() => {
				refreshTimer = null;
				void load();
			}, 150);
		}
		window.addEventListener("copod:messages-changed", scheduleLoad);
		return () => {
			cancelled = true;
			window.removeEventListener("copod:messages-changed", scheduleLoad);
			if (refreshTimer) window.clearTimeout(refreshTimer);
		};
	}, [messageId, threadId]);

	return { messages: threadId ? messages : [], loading };
}
