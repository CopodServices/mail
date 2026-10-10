"use client";

import { useEffect, useRef } from "react";
import { useCompose } from "@/components/compose/compose-context";

export function OpenDraftOnRoute({ draftId }: { draftId: string }) {
	const { openDraftComposer } = useCompose();
	const openedDraftRef = useRef<string | null>(null);

	useEffect(() => {
		if (openedDraftRef.current === draftId) return;
		openedDraftRef.current = draftId;
		openDraftComposer(draftId);
	}, [draftId, openDraftComposer]);

	return null;
}
