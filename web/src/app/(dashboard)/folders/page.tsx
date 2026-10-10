"use client";

import { useSearchParams } from "next/navigation";
import { Suspense } from "react";
import { FolderSectionRoute } from "@/components/messages/folder-section-route";
import { useCustomFolderConfig } from "@/components/messages/use-custom-folder-config";

function FoldersRoute() {
	const folderId = useSearchParams().get("folder") ?? "";
	const config = useCustomFolderConfig(folderId);
	return <FolderSectionRoute config={config} />;
}

export default function FoldersPage() {
	return (
		<Suspense fallback={null}>
			<FoldersRoute />
		</Suspense>
	);
}
