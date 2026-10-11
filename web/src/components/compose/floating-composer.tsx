"use client";

import dynamic from "next/dynamic";
import { usePathname, useRouter } from "next/navigation";
import { useCompose } from "@/components/compose/compose-context";

// The composer carries the rich-text editor and the HTML sanitizer, which the
// app only needs once a draft is opened. Loading it on demand keeps that stack
// out of every page's initial bundle instead of shipping it with the layout.
const ComposeForm = dynamic(
	() =>
		import("@/components/compose/compose-form").then((module) => ({
			default: module.ComposeForm,
		})),
	{ ssr: false },
);

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
