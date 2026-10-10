"use client";

import { useLanguage } from "@/components/language-provider";
import { ClientConnectionSettings } from "@/components/settings/client-connection-settings";

export default function SettingsClientsPage() {
	const { t } = useLanguage();
	return (
		<div className="space-y-8 py-4">
			<section className="space-y-4">
				<div>
					<h2 className="text-xl font-semibold text-neutral-900">
						{t("settings.nav.clients")}
					</h2>
				</div>
				<div className="rounded-3xl bg-white p-6">
					<ClientConnectionSettings />
				</div>
			</section>
		</div>
	);
}
