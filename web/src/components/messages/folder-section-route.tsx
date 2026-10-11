"use client";

import dynamic from "next/dynamic";
import { useSearchParams } from "next/navigation";
import { type ReactNode, Suspense } from "react";
import { MessageDetailSkeleton } from "@/components/page-skeletons";
import { MessageFolderPage } from "./message-folder-page";
import { MessageSplitLayout } from "./message-split-layout";
import type { MessageFolderConfig } from "./types";

// The reading stack (conversation thread, attachments and the HTML sanitizer)
// is heavy and only needed once a message is opened, so it loads on demand
// instead of shipping with every folder list.
const MessageDetailView = dynamic(
	() =>
		import("./message-detail/message-detail-view").then((module) => ({
			default: module.MessageDetailView,
		})),
	{ ssr: false, loading: () => <MessageDetailSkeleton /> },
);

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
