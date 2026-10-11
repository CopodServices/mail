import type { ReactNode } from "react";
import type { MessageFolderConfig } from "./types";

export type MessageDetailNavigationProviderProps = {
	children: ReactNode;
	config: MessageFolderConfig;
};

export type MessageDetailNavigationProps = {
	messageId: string;
	unread: boolean;
};

export type MessageDetailNavigationContextValue = {
	/** The folder the reader is walking, so the neighbour call asks for its order. */
	folder: string;
	folderId: string | null;
	hrefPrefix: string;
};
