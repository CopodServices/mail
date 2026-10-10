"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { sentFolderConfig } from "@/components/messages/message-folder-configs";

export default function SentPage() {
	return <FolderSectionRoute config={sentFolderConfig} />;
}
