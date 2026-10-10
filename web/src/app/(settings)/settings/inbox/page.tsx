"use client";

import { useLanguage } from "@/components/language-provider";
import { AppearanceSettings } from "@/components/settings/appearance-settings";
import { BrowserNotificationSettings } from "@/components/settings/browser-notification-settings";
import { InboxReadingLayoutSettings } from "@/components/settings/inbox-reading-layout-settings";
import { InboxShortcutsSettings } from "@/components/settings/inbox-shortcuts-settings";
import { InboxThreadingSettings } from "@/components/settings/inbox-threading-settings";
import { MailboxSignatureForm } from "@/components/settings/mailbox-signature-form";

export default function SettingsInboxPage() {
	const { t } = useLanguage();
	return (
		<div className="space-y-8 py-4">
			<section className="space-y-4">
				<div>
					<h2 className="text-xl font-semibold text-neutral-900">
						{t("settingsPage.inbox.experience")}
					</h2>
					<p className="mt-1 text-sm text-neutral-500">
						{t("settingsPage.inbox.experienceDescription")}
					</p>
				</div>
				<div className="divide-y divide-neutral-100 rounded-3xl bg-white p-6">
					<div className="py-6 first:pt-0 last:pb-0">
						<div className="mb-4">
							<h3 className="text-base font-semibold text-neutral-900">
								{t("settingsPage.inbox.appearance")}
							</h3>
							<p className="mt-1 text-sm text-neutral-500">
								{t("settingsPage.inbox.appearanceDescription")}
							</p>
						</div>
						<AppearanceSettings />
					</div>
					<div className="py-6 first:pt-0 last:pb-0">
						<div className="mb-4">
							<h3 className="text-base font-semibold text-neutral-900">
								{t("settingsPage.inbox.readingLayout")}
							</h3>
							<p className="mt-1 text-sm text-neutral-500">
								{t("settingsPage.inbox.readingLayoutDescription")}
							</p>
						</div>
						<InboxReadingLayoutSettings />
					</div>
					<div className="py-6 first:pt-0 last:pb-0">
						<div className="mb-4">
							<h3 className="text-base font-semibold text-neutral-900">
								{t("settingsPage.inbox.threading")}
							</h3>
							<p className="mt-1 text-sm text-neutral-500">
								{t("settingsPage.inbox.threadingDescription")}
							</p>
						</div>
						<InboxThreadingSettings />
					</div>
					<div className="py-6 first:pt-0 last:pb-0">
						<div className="mb-4">
							<h3 className="text-base font-semibold text-neutral-900">
								{t("settingsPage.inbox.shortcuts")}
							</h3>
							<p className="mt-1 text-sm text-neutral-500">
								{t("settingsPage.inbox.shortcutsDescription")}
							</p>
						</div>
						<InboxShortcutsSettings />
					</div>
					<div className="py-6 first:pt-0 last:pb-0">
						<div className="mb-4">
							<h3 className="text-base font-semibold text-neutral-900">
								{t("settingsPage.inbox.signature")}
							</h3>
							<p className="mt-1 text-sm text-neutral-500">
								{t("settingsPage.inbox.signatureDescription")}
							</p>
						</div>
						<MailboxSignatureForm />
					</div>
					<div className="py-6 first:pt-0 last:pb-0">
						<div className="mb-4">
							<h3 className="text-base font-semibold text-neutral-900">
								{t("settingsPage.inbox.notifications")}
							</h3>
							<p className="mt-1 text-sm text-neutral-500">
								{t("settingsPage.inbox.notificationsDescription")}
							</p>
						</div>
						<BrowserNotificationSettings />
					</div>
				</div>
			</section>
		</div>
	);
}
