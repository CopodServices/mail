"use client";

import { useEffect } from "react";
import { markMessagesReadInCaches } from "@/hooks/utils";
import { setMessageRead } from "@/lib/api/endpoints";

export function MarkAsRead({ messageId }: { messageId: string }) {
	useEffect(() => {
		setMessageRead(messageId, true)
			.then(() => {
				markMessagesReadInCaches([messageId], true);
				window.dispatchEvent(new Event("copod:messages-changed"));
			})
			.catch(() => {
				// silently fail
			});
	}, [messageId]);

	return null;
}
