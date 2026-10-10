"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { scheduledFolderConfig } from "@/components/messages/message-folder-configs";

export default function ScheduledPage() {
	return <FolderSectionRoute config={scheduledFolderConfig} />;
}
