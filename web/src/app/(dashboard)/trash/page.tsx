"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { trashFolderConfig } from "@/components/messages/message-folder-configs";

export default function TrashPage() {
	return <FolderSectionRoute config={trashFolderConfig} />;
}
