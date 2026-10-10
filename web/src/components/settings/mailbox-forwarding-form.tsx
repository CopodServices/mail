"use client";

import { useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { useSelectedMailbox } from "@/components/mailbox-provider";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { updateMailboxForwarding } from "@/lib/api/endpoints";

/**
 * The address every message arriving in this mailbox is copied to. Saving an
 * empty address turns forwarding off, and the stored value updates the selected
 * mailbox so the rest of the client sees the change at once.
 */
export function MailboxForwardingForm() {
	const { t } = useLanguage();
	const { selectedMailbox, setSelectedMailbox } = useSelectedMailbox();
	const [value, setValue] = useState(selectedMailbox?.forwardTo ?? "");
	const [saving, setSaving] = useState(false);
	const [status, setStatus] = useState<"idle" | "saved" | "failed">("idle");

	useEffect(() => {
		setValue(selectedMailbox?.forwardTo ?? "");
		setStatus("idle");
	}, [selectedMailbox?.id, selectedMailbox?.forwardTo]);

	async function save() {
		if (!selectedMailbox) return;
		const address = selectedMailbox.localPart + "@" + selectedMailbox.hostname;
		const trimmed = value.trim();
		setSaving(true);
		setStatus("idle");
		try {
			const forwardTo = await updateMailboxForwarding(
				address,
				trimmed ? trimmed : null,
			);
			setSelectedMailbox({ ...selectedMailbox, forwardTo });
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
				type="email"
				value={value}
				onChange={(event) => setValue(event.target.value)}
				placeholder="name@example.com"
				autoComplete="off"
				aria-label={t("settings.forwarding.addressLabel")}
			/>
			<p className="text-xs text-neutral-500">
				{t("settings.forwarding.hint")}
			</p>
			<div className="flex items-center gap-3">
				<Button type="button" onClick={() => void save()} disabled={saving}>
					{t("settings.forwarding.save")}
				</Button>
				{status === "saved" && (
					<span className="text-sm text-neutral-500">
						{t("settings.forwarding.saved")}
					</span>
				)}
				{status === "failed" && (
					<span className="text-sm text-red-600">
						{t("settings.forwarding.saveFailed")}
					</span>
				)}
			</div>
		</div>
	);
}
