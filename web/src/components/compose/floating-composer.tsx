"use client";

import { usePathname, useRouter } from "next/navigation";
import { useCompose } from "@/components/compose/compose-context";
import { ComposeForm } from "@/components/compose/compose-form";

export function FloatingComposer() {
	const { open, draftId, closeComposer } = useCompose();
	const pathname = usePathname();
	const router = useRouter();
	if (!open) return null;
	return (
		<ComposeForm
			key={draftId ?? "new"}
			mode="popup"
			draftIdToLoad={draftId}
			onClose={() => {
				closeComposer();
				if (/^\/drafts\/[^/]+\/?$/.test(pathname)) router.replace("/drafts");
			}}
		/>
	);
}
