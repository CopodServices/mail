"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { archivedFolderConfig } from "@/components/messages/message-folder-configs";

export default function ArchivedPage() {
	return <FolderSectionRoute config={archivedFolderConfig} />;
}
