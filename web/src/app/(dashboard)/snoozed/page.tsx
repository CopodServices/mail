"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { snoozedFolderConfig } from "@/components/messages/message-folder-configs";

export default function SnoozedPage() {
	return <FolderSectionRoute config={snoozedFolderConfig} />;
}
