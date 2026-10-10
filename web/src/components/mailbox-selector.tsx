"use client";

import { Check, ChevronDown, Inbox, Settings } from "lucide-react";
import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { useSelectedMailbox } from "@/components/mailbox-provider";
import { Skeleton } from "@/components/ui/skeleton";
import {
	getAccountInitial,
	getMailboxAddress,
	getMailboxName,
} from "./mailbox-selector-utils";

/**
 * Mailbox picker. The client has no accounts or sessions yet: it just switches
 * between the mailboxes the API exposes for this deployment.
 */
export function MailboxSelector() {
	const { t } = useLanguage();
	const { selectedMailbox, setSelectedMailbox, mailboxes, isLoading } =
		useSelectedMailbox();
	const [open, setOpen] = useState(false);
	const ref = useRef<HTMLDivElement>(null);

	useEffect(() => {
		function onPointerDown(event: PointerEvent) {
			if (!ref.current?.contains(event.target as Node)) setOpen(false);
		}
		document.addEventListener("pointerdown", onPointerDown);
		return () => document.removeEventListener("pointerdown", onPointerDown);
	}, []);

	if (isLoading && !selectedMailbox) {
		return <Skeleton className="h-9 w-9 rounded-full" />;
	}

	const name = selectedMailbox ? getMailboxName(selectedMailbox) : "";
	const address = selectedMailbox ? getMailboxAddress(selectedMailbox) : "";

	return (
		<div ref={ref} className="relative">
			<button
				type="button"
				onClick={() => setOpen((value) => !value)}
				className="flex items-center gap-2 rounded-full py-1 pl-1 pr-2 transition-colors hover:bg-neutral-200"
				aria-label={t("account.openMenu")}
				aria-expanded={open}
			>
				<span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-blue-600 text-sm font-semibold text-white">
					{getAccountInitial(name || address || "M")}
				</span>
				<ChevronDown size={16} className="text-neutral-500" />
			</button>

			{open && (
				<div className="absolute right-0 z-50 mt-2 w-72 overflow-hidden rounded-2xl border border-neutral-200 bg-white shadow-lg">
					<div className="border-b border-neutral-100 px-4 py-3">
						<p className="truncate text-sm font-semibold text-neutral-900">
							{name}
						</p>
						<p className="truncate text-xs text-neutral-500">{address}</p>
					</div>
					<div className="max-h-72 overflow-y-auto">
						{mailboxes.map((mailbox) => {
							const active = mailbox.id === selectedMailbox?.id;
							return (
								<button
									key={mailbox.id}
									type="button"
									onClick={() => {
										setSelectedMailbox(mailbox);
										setOpen(false);
									}}
									className="flex w-full items-center gap-3 px-4 py-3 text-left transition-colors hover:bg-neutral-50"
								>
									<Inbox size={16} className="shrink-0 text-neutral-400" />
									<span className="min-w-0 flex-1">
										<span className="block truncate text-sm text-neutral-900">
											{getMailboxName(mailbox)}
										</span>
										<span className="block truncate text-xs text-neutral-500">
											{getMailboxAddress(mailbox)}
										</span>
									</span>
									{active && (
										<Check size={16} className="shrink-0 text-blue-600" />
									)}
								</button>
							);
						})}
					</div>
					<Link
						href="/settings/inbox"
						onClick={() => setOpen(false)}
						className="flex items-center gap-3 border-t border-neutral-100 px-4 py-3 text-sm text-neutral-700 hover:bg-neutral-50"
					>
						<Settings size={16} className="text-neutral-500" />
						{t("navigation.settings")}
					</Link>
				</div>
			)}
		</div>
	);
}
