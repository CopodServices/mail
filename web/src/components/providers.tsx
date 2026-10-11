"use client";

import { BrandingProvider } from "@/components/branding-provider";
import { NewMessagePopup } from "@/components/new-message-popup";
import { ThemeSync } from "@/components/theme-sync";
import { useMessagePolling } from "@/hooks/use-message-polling";

export function Providers({ children }: { children: React.ReactNode }) {
	const realtime = useMessagePolling();

	return (
		<BrandingProvider>
			{children}
			<ThemeSync />
			{realtime.notification && (
				<NewMessagePopup
					notification={realtime.notification}
					onDismiss={realtime.dismissNotification}
				/>
			)}
		</BrandingProvider>
	);
}
