"use client";

import { useSearchParams } from "next/navigation";
import { type ReactNode, Suspense } from "react";
import { MessageDetailView } from "./message-detail/message-detail-view";
import { MessageFolderPage } from "./message-folder-page";
import { MessageSplitLayout } from "./message-split-layout";
import type { MessageFolderConfig } from "./types";

type Props = {
	config: MessageFolderConfig;
	/** Rendered when no message is selected. Defaults to the folder list. */
	renderList?: () => ReactNode;
	/** Rendered in the reading pane when a message is selected. Defaults to the detail view. */
	renderDetail?: (messageId: string) => ReactNode;
};

function FolderSection({ config, renderList, renderDetail }: Props) {
	const messageId = useSearchParams().get("message") ?? undefined;
	return (
		<MessageSplitLayout config={config} selectedMessageId={messageId}>
			{messageId ? (
				renderDetail ? (
					renderDetail(messageId)
				) : (
					<MessageDetailView messageId={messageId} />
				)
			) : renderList ? (
				renderList()
			) : (
				<MessageFolderPage config={config} />
			)}
		</MessageSplitLayout>
	);
}

/**
 * A mail folder route. The bundle is a static export, so the selected message
 * travels in the query string (?message=) and the whole view is client-side.
 */
export function FolderSectionRoute(props: Props) {
	return (
		<Suspense fallback={null}>
			<FolderSection {...props} />
		</Suspense>
	);
}
