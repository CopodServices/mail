"use client";

import clsx from "clsx";
import { Menu } from "lucide-react";
import type { ReactNode } from "react";
import { useLanguage } from "./language-provider";
import { useSidebar } from "./sidebar-state";

export function SidebarAside({
	children,
	className,
}: {
	children: ReactNode;
	className?: string;
}) {
	const { mobile, mobileOpen, toggle } = useSidebar();
	const hidden = mobile && !mobileOpen;

	return (
		<>
			<div
				onClick={toggle}
				aria-hidden="true"
				className={clsx(
					"fixed inset-0 z-[100] bg-black/40 transition-opacity duration-200 ease-out motion-reduce:transition-none md:hidden",
					mobileOpen ? "opacity-100" : "pointer-events-none opacity-0",
				)}
			/>
			<aside
				aria-hidden={hidden}
				inert={hidden}
				className={clsx(
					"fixed inset-y-0 left-0 z-[110] w-[min(85vw,300px)] min-h-0 min-w-0 bg-[#f6f8fc] transition-transform duration-200 ease-out motion-reduce:transition-none",
					"md:relative md:z-30 md:w-[var(--sidebar-width)] md:translate-x-0 md:shadow-none",
					mobileOpen ? "translate-x-0 shadow-xl" : "-translate-x-full",
					className,
				)}
			>
				{children}
			</aside>
		</>
	);
}

export function MobileMenuButton({ className }: { className?: string }) {
	const { t } = useLanguage();
	const { mobile, mobileOpen, toggle } = useSidebar();
	if (!mobile) return null;
	return (
		<button
			type="button"
			onClick={toggle}
			aria-label={t("navigation.openMenu")}
			aria-expanded={mobileOpen}
			className={clsx(
				"flex h-10 w-10 shrink-0 items-center justify-center rounded-full text-neutral-600 hover:bg-neutral-200 md:hidden",
				className,
			)}
		>
			<Menu size={20} />
		</button>
	);
}
