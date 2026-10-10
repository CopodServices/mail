"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { starredFolderConfig } from "@/components/messages/message-folder-configs";

export default function StarredPage() {
	return <FolderSectionRoute config={starredFolderConfig} />;
}
