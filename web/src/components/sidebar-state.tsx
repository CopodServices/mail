"use client";

import { usePathname } from "next/navigation";
import {
	createContext,
	useContext,
	useEffect,
	useLayoutEffect,
	useState,
} from "react";
import { readInitialColumnWidth } from "./column-width-preferences";
import { isMobileViewport, useIsMobile } from "./sidebar-mobile-utils";
import type { SidebarProviderProps, SidebarState } from "./sidebar-state-types";
import {
	readInitialSidebarMinimal,
	saveInitialSidebarMinimal,
	syncInitialSidebarWidth,
} from "./sidebar-state-utils";

const SidebarContext = createContext<SidebarState>({
	minimal: false,
	width: 260,
	userId: null,
	toggle: () => undefined,
	setWidth: () => undefined,
	setForcedMinimal: () => undefined,
	mobile: false,
	mobileOpen: false,
});

export function SidebarProvider({
	children,
	expandedWidth = 260,
	mobileOverlay = false,
}: SidebarProviderProps) {
	const [minimal, setMinimal] = useState(false);
	const [forcedMinimal, setForcedMinimal] = useState(false);
	const [width, setWidth] = useState(expandedWidth);
	const [widthReady, setWidthReady] = useState(false);
	const [userId] = useState<string | null>(null);
	const [storageKey] = useState<string | null>(null);
	const pathname = usePathname();
	const mobile = useIsMobile();
	const [mobileOpen, setMobileOpen] = useState(false);
	// Overlay layouts have no minified rail on phones: the menu is either fully shown over the page or hidden.
	const overlay = mobileOverlay && mobile;

	useEffect(() => {
		setMobileOpen(false);
	}, [pathname, mobile]);

	// Collapse on first load and after every navigation on phones, so the overlay never sticks open.
	useEffect(() => {
		if (mobile) setMinimal(true);
	}, [pathname, mobile]);

	useEffect(() => {
		if (!mobile) setMinimal(readInitialSidebarMinimal());
	}, [mobile]);

	useLayoutEffect(() => {
		setMinimal(readInitialSidebarMinimal());
		setWidth(readInitialColumnWidth("sidebar", expandedWidth, 200, 480));
		const frame = requestAnimationFrame(() => setWidthReady(true));
		return () => cancelAnimationFrame(frame);
	}, [expandedWidth]);

	useLayoutEffect(() => {
		syncInitialSidebarWidth(width, minimal);
	}, [width, minimal]);

	useEffect(() => {}, []);

	function toggle() {
		if (overlay) {
			setMobileOpen((current) => !current);
			return;
		}
		if (forcedMinimal) {
			setForcedMinimal(false);
			setMinimal(false);
			if (!isMobileViewport()) {
				saveInitialSidebarMinimal(false);
				if (storageKey) localStorage.setItem(storageKey, "false");
			}
			return;
		}
		setMinimal((current) => {
			const next = !current;
			if (!isMobileViewport()) {
				saveInitialSidebarMinimal(next);
				if (storageKey) localStorage.setItem(storageKey, String(next));
			}
			return next;
		});
	}

	return (
		<SidebarContext.Provider
			value={{
				minimal: !overlay && (minimal || forcedMinimal),
				width,
				userId,
				toggle,
				setWidth,
				setForcedMinimal,
				mobile: overlay,
				mobileOpen,
			}}
		>
			<div
				className="h-full"
				style={
					{
						"--sidebar-width": widthReady
							? `${minimal || forcedMinimal ? 72 : width}px`
							: `var(--persisted-sidebar-width, ${expandedWidth}px)`,
						"--sidebar-transition-duration": widthReady ? "200ms" : "0ms",
					} as React.CSSProperties
				}
			>
				{children}
			</div>
		</SidebarContext.Provider>
	);
}

export function useSidebar() {
	return useContext(SidebarContext);
}
