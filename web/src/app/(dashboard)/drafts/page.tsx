"use client";

import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { draftsFolderConfig } from "@/components/messages/message-folder-configs";
import { MessageFolderPage } from "@/components/messages/message-folder-page";
import { OpenDraftOnRoute } from "@/components/messages/open-draft-on-route";

export default function DraftsPage() {
	return (
		<FolderSectionRoute
			config={draftsFolderConfig}
			renderDetail={(messageId) => (
				<>
					<MessageFolderPage config={draftsFolderConfig} />
					<OpenDraftOnRoute draftId={messageId} />
				</>
			)}
		/>
	);
}
