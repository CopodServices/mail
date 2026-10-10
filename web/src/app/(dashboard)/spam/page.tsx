"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { spamFolderConfig } from "@/components/messages/message-folder-configs";

export default function SpamPage() {
	return <FolderSectionRoute config={spamFolderConfig} />;
}
