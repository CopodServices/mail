"use client";

import { HardDrive, Keyboard, LogOut } from "lucide-react";
import { useRouter } from "next/navigation";
import { useMailboxStorage } from "@/hooks/use-mailbox-storage";
import { signOut } from "@/lib/api/session";
import { LOGIN_PATH } from "@/lib/api/session-utils";
import { cn } from "@/lib/utils";
import { useLanguage } from "./language-provider";
import { useSelectedMailbox } from "./mailbox-provider";
import { useShortcuts } from "./shortcuts";
import { storageLabel, storagePercent } from "./sidebar-footer-utils";
import { useSidebar } from "./sidebar-state";
import { Tooltip } from "./ui/tooltip";

/** Sidebar footer: the mailbox storage meter, quick keys and the account menu. */
export function SidebarFooter() {
	const { t } = useLanguage();
	const router = useRouter();
	const { minimal } = useSidebar();
	const { selectedMailbox, isLoading } = useSelectedMailbox();
	const storage = useMailboxStorage(selectedMailbox?.id, !isLoading);
	const { openHelpModal, shortcutsEnabled, shortcutsPreferenceLoading } =
		useShortcuts();

	const label = storage
		? storageLabel(
				storage.usedBytes,
				storage.quotaBytes,
				t("storage.unlimited"),
			)
		: null;
	const percent = storage
		? storagePercent(storage.usedBytes, storage.quotaBytes)
		: null;

	// The icon rail has no room for the meter; keep the tooltip instead.
	if (minimal) {
		return label ? (
			<Tooltip
				label={`${t("storage.title")}: ${label}`}
				placement="right"
				className="w-full justify-center"
			>
				<span className="flex h-8 w-8 items-center justify-center rounded-full text-neutral-500">
					<HardDrive className="h-4 w-4 text-neutral-400" />
				</span>
			</Tooltip>
		) : null;
	}

	return (
		<div className="px-3 pt-3 flex flex-col gap-2">
			{label ? (
				<div className="rounded-lg bg-neutral-100 px-2.5 py-2">
					<div className="flex items-center justify-between gap-2 text-xs text-neutral-500">
						<span className="flex items-center gap-1.5">
							<HardDrive className="h-3.5 w-3.5 text-neutral-400" />
							{t("storage.title")}
						</span>
						<span
							className="truncate font-medium text-neutral-700 tabular-nums"
							title={label}
						>
							{label}
						</span>
					</div>
					{percent !== null ? (
						<div
							role="progressbar"
							aria-valuemin={0}
							aria-valuemax={100}
							aria-valuenow={percent}
							aria-label={`${t("storage.title")}: ${label}`}
							className="mt-1.5 h-1.5 w-full overflow-hidden rounded-full bg-neutral-200"
						>
							<div
								className={cn(
									"h-full rounded-full transition-[width] duration-200",
									percent >= 90 ? "bg-red-500" : "bg-blue-500",
								)}
								style={{ width: `${percent}%` }}
							/>
						</div>
					) : null}
				</div>
			) : null}

			{shortcutsEnabled && !shortcutsPreferenceLoading && (
				<button
					type="button"
					onClick={openHelpModal}
					className="flex items-center justify-between w-full px-2.5 py-1.5 text-xs text-neutral-500 hover:text-neutral-800 hover:bg-neutral-200/60 rounded-lg transition-colors"
				>
					<span className="flex items-center gap-1.5">
						<Keyboard className="w-3.5 h-3.5 text-neutral-400" />
						{t("navigation.shortcuts")}
					</span>
					<kbd className="px-1.5 py-0.5 font-mono text-[10px] bg-white border border-neutral-200 rounded text-neutral-500 shadow-2xs">
						?
					</kbd>
				</button>
			)}
			<button
				type="button"
				onClick={() => {
					void signOut().finally(() => router.replace(LOGIN_PATH));
				}}
				className="flex items-center gap-1.5 w-full px-2.5 py-1.5 text-xs text-neutral-500 hover:text-neutral-800 hover:bg-neutral-200/60 rounded-lg transition-colors"
			>
				<LogOut className="w-3.5 h-3.5 text-neutral-400" />
				{t("session.signOut")}
			</button>
			<PoweredBy />
		</div>
	);
}

/**
 * Footer source link. The client reimplements the Mailflare mail client UI;
 * the upstream project is credited in /NOTICE and the repository README rather
 * than in the product surface. The link keeps the AGPL-3.0 section 13
 * network-source obligation satisfied.
 */
function PoweredBy({ className = "px-1" }: { className?: string }) {
	const sourceUrl =
		process.env.NEXT_PUBLIC_SOURCE_URL ??
		"https://github.com/copodservices/mail";
	return (
		<p className={className + " text-[11px] text-neutral-400"}>
			<a
				href={sourceUrl}
				target="_blank"
				rel="noreferrer"
				className="hover:underline text-neutral-500"
				title="Source code for this mail client (AGPL-3.0)"
			>
				Source
			</a>
		</p>
	);
}
