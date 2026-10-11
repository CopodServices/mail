"use client";

import { ChevronLeft, ChevronRight } from "lucide-react";
import { useRouter } from "next/navigation";
import { createContext, useContext, useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { useSelectedMailbox } from "@/components/mailbox-provider";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { apiFetch } from "@/lib/api/client";
import type { MessageNeighbors } from "@/lib/api/types";
import { messageHref } from "@/lib/mail-routes";
import type {
	MessageDetailNavigationContextValue,
	MessageDetailNavigationProps,
	MessageDetailNavigationProviderProps,
} from "./message-detail-navigation-types";

const NavigationContext =
	createContext<MessageDetailNavigationContextValue | null>(null);

export function MessageDetailNavigationProvider({
	children,
	config,
}: MessageDetailNavigationProviderProps) {
	return (
		<NavigationContext.Provider
			value={{
				folder: config.folder,
				folderId: config.folderId ?? null,
				hrefPrefix: config.hrefPrefix,
			}}
		>
			{children}
		</NavigationContext.Provider>
	);
}

/**
 * The newer and older neighbours of the open message and its unread position,
 * read from the API one message at a time instead of the whole folder.
 */
export function MessageDetailNavigation({
	messageId,
	unread,
}: MessageDetailNavigationProps) {
	const { t } = useLanguage();
	const context = useContext(NavigationContext);
	const router = useRouter();
	const { selectedMailbox, isLoading: mailboxesLoading } = useSelectedMailbox();
	const [neighbors, setNeighbors] = useState<MessageNeighbors | null>(null);

	const folder = context?.folderId ?? context?.folder ?? null;
	const mailboxId = selectedMailbox?.id;
	useEffect(() => {
		if (mailboxesLoading || !folder) return;
		let cancelled = false;
		const params = new URLSearchParams({ folder });
		if (mailboxId) params.set("mailbox_id", mailboxId);
		// Opening the message marks it read; telling the API how the reader sees
		// it keeps the unread position from moving under them.
		if (unread) params.set("unread", "true");
		void apiFetch(`/api/messages/${messageId}/neighbors?${params.toString()}`)
			.then((response) => {
				if (!response.ok) throw new Error("Unable to load message navigation");
				return response.json() as Promise<{ data?: MessageNeighbors }>;
			})
			.then((payload) => {
				if (!cancelled) setNeighbors(payload.data ?? null);
			})
			.catch(() => {
				if (!cancelled) setNeighbors(null);
			});
		return () => {
			cancelled = true;
		};
	}, [folder, mailboxId, mailboxesLoading, messageId, unread]);

	if (!context) return null;
	const previousId = neighbors?.previous_id ?? null;
	const nextId = neighbors?.next_id ?? null;
	const totalUnread = neighbors?.unread_total ?? 0;
	const unreadOrder = neighbors?.unread_position ?? 0;

	return (
		<div className="ml-auto flex shrink-0 items-center gap-2 text-neutral-500">
			{totalUnread > 0 && (
				<span className="mr-2 whitespace-nowrap text-xs" aria-live="polite">
					{t("message.nav.unreadPosition", {
						position: unreadOrder,
						total: totalUnread,
					})}
				</span>
			)}
			<Tooltip label={t("message.nav.newer")}>
				<Button
					type="button"
					variant="ghost"
					size="roundedSM"
					aria-label={t("message.nav.newer")}
					disabled={!previousId}
					onClick={() =>
						previousId &&
						router.push(messageHref(context.hrefPrefix, previousId))
					}
				>
					<ChevronLeft size={18} />
				</Button>
			</Tooltip>
			<Tooltip label={t("message.nav.older")}>
				<Button
					type="button"
					variant="ghost"
					size="roundedSM"
					aria-label={t("message.nav.older")}
					disabled={!nextId}
					onClick={() =>
						nextId && router.push(messageHref(context.hrefPrefix, nextId))
					}
				>
					<ChevronRight size={18} />
				</Button>
			</Tooltip>
		</div>
	);
}
