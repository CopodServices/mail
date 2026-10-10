"use client";

import { Folder } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useSelectedMailbox } from "@/components/mailbox-provider";
import { fetchFolders } from "@/lib/api/endpoints";
import { folderListHref } from "@/lib/mail-routes";
import type { CustomFolderSummary } from "./custom-folder-config-types";
import type { MessageFolderConfig } from "./types";

export function useCustomFolderConfig(folderId: string): MessageFolderConfig {
	const { selectedMailbox } = useSelectedMailbox();
	const [folderName, setFolderName] = useState("Folder");

	useEffect(() => {
		if (!selectedMailbox?.id) return;
		let cancelled = false;
		fetchFolders(selectedMailbox.id)
			.then((items: CustomFolderSummary[]) => {
				if (cancelled) return;
				const folder = items.find((item) => item.id === folderId);
				if (folder) setFolderName(folder.name);
			})
			.catch(() => {});

		return () => {
			cancelled = true;
		};
	}, [folderId, selectedMailbox?.id]);

	return useMemo(
		() => ({
			folder: "inbox",
			folderId,
			title: folderName,
			emptyText: "No emails in this folder",
			hrefPrefix: folderListHref(folderId),
			icon: Folder,
			showRowBadge: false,
		}),
		[folderId, folderName],
	);
}
