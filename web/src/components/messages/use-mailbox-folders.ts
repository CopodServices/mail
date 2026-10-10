"use client";

import { useEffect, useState } from "react";
import type { CustomFolder } from "@/components/dashboard-nav-types";
import { fetchFolders } from "@/lib/api/endpoints";

// Shared across toolbars so reopening a menu shows the folders immediately instead of popping in.
const cache = new Map<string, CustomFolder[]>();

export function useMailboxFolders(
	mailboxId: string | undefined,
): CustomFolder[] {
	const [fetched, setFetched] = useState<{
		mailboxId: string;
		folders: CustomFolder[];
	} | null>(null);

	useEffect(() => {
		if (!mailboxId) return;
		let cancelled = false;
		fetchFolders(mailboxId)
			.then((items) => {
				const folders: CustomFolder[] = items
					.filter((item) => item.kind === "custom")
					.map((item) => ({
						id: item.id,
						name: item.name,
						color: item.color ?? "#2563eb",
					}));
				cache.set(mailboxId, folders);
				if (!cancelled) setFetched({ mailboxId, folders });
			})
			.catch(() => undefined);
		return () => {
			cancelled = true;
		};
	}, [mailboxId]);

	if (!mailboxId) return [];
	return fetched?.mailboxId === mailboxId
		? fetched.folders
		: (cache.get(mailboxId) ?? []);
}
