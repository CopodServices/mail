"use client";

import { useLanguage } from "@/components/language-provider";
import { SpamSettingsForm } from "@/components/settings/spam-settings-form";

export default function SettingsSpamPage() {
	const { t } = useLanguage();
	return (
		<div className="space-y-8 py-4">
			<section className="space-y-4">
				<div>
					<h2 className="text-xl font-semibold text-neutral-900">
						{t("settings.nav.spam")}
					</h2>
					<p className="mt-1 text-sm text-neutral-500">
						{t("settings.spam.description")}
					</p>
				</div>
				<div className="rounded-3xl bg-white p-6">
					<SpamSettingsForm />
				</div>
			</section>
		</div>
	);
}
