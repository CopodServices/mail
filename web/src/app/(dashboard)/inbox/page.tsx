"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { inboxFolderConfig } from "@/components/messages/message-folder-configs";

export default function InboxPage() {
	return <FolderSectionRoute config={inboxFolderConfig} />;
}
