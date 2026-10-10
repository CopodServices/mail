"use client";

import { Keyboard, LogOut } from "lucide-react";
import { useRouter } from "next/navigation";
import { signOut } from "@/lib/api/session";
import { LOGIN_PATH } from "@/lib/api/session-utils";
import { useLanguage } from "./language-provider";
import { useShortcuts } from "./shortcuts";
import { useSidebar } from "./sidebar-state";

export function SidebarFooter() {
	const { t } = useLanguage();
	const router = useRouter();
	const { minimal } = useSidebar();
	const { openHelpModal, shortcutsEnabled, shortcutsPreferenceLoading } =
		useShortcuts();
	if (minimal) return null;

	return (
		<div className="px-3 pt-3 flex flex-col gap-2">
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
