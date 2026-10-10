"use client";

import { useRef } from "react";
import { useLanguage } from "@/components/language-provider";
import { saveColumnWidth } from "./column-width-preferences";
import { useSidebar } from "./sidebar-state";
import { ResizeHandle } from "./ui/resize-handle";

export function SidebarResizeBoundary() {
	const { t } = useLanguage();
	const { minimal, width, userId, setWidth } = useSidebar();
	const startWidth = useRef(width);
	const resizedWidth = useRef(width);

	// Hidden on phones, where the sidebar is a fixed icon rail and the handle would swallow touch scrolls.
	return (
		<div className="hidden md:block">
			<ResizeHandle
				label={t("layout.resizeMenu")}
				onResizeStart={() => {
					startWidth.current = width;
					resizedWidth.current = width;
				}}
				onResize={(delta) => {
					if (minimal) return;
					resizedWidth.current = Math.max(
						200,
						Math.min(480, window.innerWidth - 570, startWidth.current + delta),
					);
					setWidth(resizedWidth.current);
				}}
				onResizeEnd={() =>
					saveColumnWidth(userId, "sidebar", resizedWidth.current)
				}
			/>
		</div>
	);
}
