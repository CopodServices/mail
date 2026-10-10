"use client";

import { useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { useSelectedMailbox } from "@/components/mailbox-provider";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { apiFetch } from "@/lib/api/client";

/**
 * The signature the composer appends to new messages from this mailbox.
 * Saving updates the selected mailbox so compose picks it up immediately.
 */
export function MailboxSignatureForm() {
	const { t } = useLanguage();
	const { selectedMailbox, setSelectedMailbox } = useSelectedMailbox();
	const [value, setValue] = useState(selectedMailbox?.signature ?? "");
	const [saving, setSaving] = useState(false);
	const [status, setStatus] = useState<"idle" | "saved" | "failed">("idle");

	useEffect(() => {
		setValue(selectedMailbox?.signature ?? "");
		setStatus("idle");
	}, [selectedMailbox?.id, selectedMailbox?.signature]);

	async function save() {
		if (!selectedMailbox) return;
		const signature = value.trim() ? value : null;
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
					body: JSON.stringify({ signature }),
				},
			);
			if (!response.ok) throw new Error("save failed");
			setSelectedMailbox({ ...selectedMailbox, signature });
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
			<Textarea
				value={value}
				onChange={(event) => setValue(event.target.value)}
				placeholder={t("settings.signature.placeholder")}
				rows={4}
			/>
			<div className="flex items-center gap-3">
				<Button type="button" onClick={() => void save()} disabled={saving}>
					{t("settings.signature.save")}
				</Button>
				{status === "saved" && (
					<span className="text-sm text-neutral-500">
						{t("settings.signature.saved")}
					</span>
				)}
				{status === "failed" && (
					<span className="text-sm text-red-600">
						{t("settings.signature.saveFailed")}
					</span>
				)}
			</div>
		</div>
	);
}
