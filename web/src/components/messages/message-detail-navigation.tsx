"use client";

import { ChevronLeft, ChevronRight } from "lucide-react";
import { useRouter } from "next/navigation";
import {
	createContext,
	useCallback,
	useContext,
	useEffect,
	useState,
} from "react";
import { useLanguage } from "@/components/language-provider";
import { useSelectedMailbox } from "@/components/mailbox-provider";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { apiFetch } from "@/lib/api/client";
import type { MessageNavigationEntry } from "@/lib/mail-api/types";
import { messageHref } from "@/lib/mail-routes";
import type {
	MessageDetailNavigationContextValue,
	MessageDetailNavigationProps,
	MessageDetailNavigationProviderProps,
	MessageDetailNavigationSnapshot,
	OpenedUnreadMessages,
} from "./message-detail-navigation-types";
import {
	getMessageDetailNavigationState,
	takeOpenedUnreadMessage,
} from "./message-detail-navigation-utils";
import { useConversationView } from "./use-conversation-view";

const NavigationContext =
	createContext<MessageDetailNavigationContextValue | null>(null);

export function MessageDetailNavigationProvider({
	children,
	config,
}: MessageDetailNavigationProviderProps) {
	const { selectedMailbox, isLoading: mailboxesLoading } = useSelectedMailbox();
	const [conversationView] = useConversationView();
	const grouped = conversationView && config.folder !== "drafts";
	const mailboxId = selectedMailbox?.id;
	const key = `${config.folder}:${config.folderId ?? ""}:${mailboxId ?? "all"}:${grouped}`;
	const [snapshot, setSnapshot] = useState<MessageDetailNavigationSnapshot>({
		key: "",
		entries: [],
	});
	const [openedUnread, setOpenedUnread] = useState<OpenedUnreadMessages>({
		key: "",
		ids: new Set(),
	});
	const recordUnread = useCallback(
		(messageId: string) => {
			setOpenedUnread((current) => {
				const ids = new Set(current.key === key ? current.ids : []);
				ids.add(messageId);
				return { key, ids };
			});
		},
		[key],
	);

	useEffect(() => {
		const messageId = takeOpenedUnreadMessage();
		if (messageId) recordUnread(messageId);
	}, [recordUnread]);

	useEffect(() => {
		if (mailboxesLoading) return;
		let cancelled = false;
		// The API filters by folder; custom folders travel as their folder id.
		const params = new URLSearchParams({
			folder: config.folderId ?? config.folder,
		});
		if (mailboxId) params.set("mailbox_id", mailboxId);
		void apiFetch(`/api/messages/navigation?${params.toString()}`)
			.then((response) => {
				if (!response.ok) throw new Error("Unable to load message navigation");
				return response.json() as Promise<{
					data?: MessageNavigationEntry[];
				}>;
			})
			.then((response) => {
				if (!cancelled) setSnapshot({ key, entries: response.data ?? [] });
			})
			.catch(() => {
				if (!cancelled) setSnapshot({ key, entries: [] });
			});
		return () => {
			cancelled = true;
		};
	}, [
		config.folder,
		config.folderId,
		grouped,
		key,
		mailboxId,
		mailboxesLoading,
	]);

	const unreadIds = openedUnread.key === key ? openedUnread.ids : null;
	const entries =
		snapshot.key === key
			? snapshot.entries.map((entry) => ({
					...entry,
					unread: entry.unread || !!unreadIds?.has(entry.id),
				}))
			: [];
	return (
		<NavigationContext.Provider
			value={{ entries, hrefPrefix: config.hrefPrefix, recordUnread }}
		>
			{children}
		</NavigationContext.Provider>
	);
}

export function MessageDetailNavigation({
	messageId,
	unread,
}: MessageDetailNavigationProps) {
	const { t } = useLanguage();
	const context = useContext(NavigationContext);
	const router = useRouter();
	useEffect(() => {
		if (unread) context?.recordUnread(messageId);
	}, [context?.recordUnread, messageId, unread]);
	if (!context) return null;
	const { previousId, nextId, unreadOrder, totalUnread } =
		getMessageDetailNavigationState(context.entries, messageId);

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
