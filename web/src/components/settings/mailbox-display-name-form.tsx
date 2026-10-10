"use client";

import { useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { useSelectedMailbox } from "@/components/mailbox-provider";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { apiFetch } from "@/lib/api/client";

/**
 * The name recipients see beside this mailbox's address in the From header.
 * Saving updates the selected mailbox so the composer picks it up immediately.
 */
export function MailboxDisplayNameForm() {
	const { t } = useLanguage();
	const { selectedMailbox, setSelectedMailbox } = useSelectedMailbox();
	const [value, setValue] = useState(selectedMailbox?.displayName ?? "");
	const [saving, setSaving] = useState(false);
	const [status, setStatus] = useState<"idle" | "saved" | "failed">("idle");

	useEffect(() => {
		setValue(selectedMailbox?.displayName ?? "");
		setStatus("idle");
	}, [selectedMailbox?.id, selectedMailbox?.displayName]);

	async function save() {
		if (!selectedMailbox) return;
		const displayName = value.trim() ? value.trim() : null;
		setSaving(true);
		setStatus("idle");
		try {
			const address =
				selectedMailbox.localPart + "@" + selectedMailbox.hostname;
			const response = await apiFetch(
				"/api/mailboxes/" + encodeURIComponent(address),
				{
					method: "PATCH",
					headers: { "Content-Type": "application/json" },
					body: JSON.stringify({ display_name: displayName }),
				},
			);
			if (!response.ok) throw new Error("save failed");
			setSelectedMailbox({ ...selectedMailbox, displayName });
			setStatus("saved");
		} catch {
			setStatus("failed");
		} finally {
			setSaving(false);
		}
	}

	if (!selectedMailbox) return null;

	return (
		<div className="space-y-3">
			<Input
				value={value}
				onChange={(event) => setValue(event.target.value)}
				placeholder={t("settings.displayName.placeholder")}
				maxLength={120}
			/>
			<div className="flex items-center gap-3">
				<Button type="button" onClick={() => void save()} disabled={saving}>
					{t("settings.displayName.save")}
				</Button>
				{status === "saved" && (
					<span className="text-sm text-neutral-500">
						{t("settings.displayName.saved")}
					</span>
				)}
				{status === "failed" && (
					<span className="text-sm text-red-600">
						{t("settings.displayName.saveFailed")}
					</span>
				)}
			</div>
		</div>
	);
}
