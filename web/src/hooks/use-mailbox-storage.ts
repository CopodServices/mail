"use client";

import { useEffect, useState } from "react";
import { fetchMailboxStorage } from "@/lib/api/endpoints";

const REFRESH_DEBOUNCE_MS = 150;

export type MailboxStorageState = {
	usedBytes: number;
	quotaBytes: number;
};

/**
 * Storage used and quota for one mailbox, reloaded when mail changes. The
 * mailbox list is the source of truth (a session sees only its own mailbox).
 */
export function useMailboxStorage(
	mailboxId?: string | null,
	enabled = true,
): MailboxStorageState | null {
	const [storage, setStorage] = useState<MailboxStorageState | null>(null);

	useEffect(() => {
		if (!enabled || !mailboxId) return;
		let cancelled = false;
		let refreshTimer: number | null = null;
		let requestNumber = 0;

		async function load() {
			const currentRequest = ++requestNumber;
			try {
				const items = await fetchMailboxStorage();
				const found = items.find((item) => item.mailboxId === mailboxId);
				if (!cancelled && currentRequest === requestNumber && found) {
					setStorage({
						usedBytes: found.usedBytes,
						quotaBytes: found.quotaBytes,
					});
				}
			} catch {
				// Keep the last known value; the meter is decorative.
			}
		}

		function refresh() {
			if (document.visibilityState !== "visible") return;
			if (refreshTimer) window.clearTimeout(refreshTimer);
			refreshTimer = window.setTimeout(() => {
				refreshTimer = null;
				void load();
			}, REFRESH_DEBOUNCE_MS);
		}

		void load();
		window.addEventListener("copod:messages-changed", refresh);
		window.addEventListener("copod:message-counts-changed", refresh);

		return () => {
			cancelled = true;
			window.removeEventListener("copod:messages-changed", refresh);
			window.removeEventListener("copod:message-counts-changed", refresh);
			if (refreshTimer) window.clearTimeout(refreshTimer);
		};
	}, [enabled, mailboxId]);

	return storage;
}
