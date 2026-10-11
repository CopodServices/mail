"use client";

import { useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
	fetchSpamSettings,
	updateSpamList,
	updateSpamThreshold,
} from "@/lib/api/endpoints";
import type { SpamSettings } from "@/lib/api/types";

/**
 * Per-mailbox antispam settings: the junk threshold (blank follows the global
 * default) and the sender allow/block lists the engine consults before filing.
 */
export function SpamSettingsForm() {
	const { t } = useLanguage();
	const [settings, setSettings] = useState<SpamSettings | null>(null);
	const [threshold, setThreshold] = useState("");
	const [address, setAddress] = useState("");
	const [target, setTarget] = useState<"allow" | "block">("block");
	const [status, setStatus] = useState<"idle" | "saved" | "failed">("idle");
	const [busy, setBusy] = useState(false);

	useEffect(() => {
		void fetchSpamSettings()
			.then((value) => {
				setSettings(value);
				setThreshold(value.threshold == null ? "" : String(value.threshold));
			})
			.catch(() => setStatus("failed"));
	}, []);

	async function saveThreshold() {
		const trimmed = threshold.trim();
		const value = trimmed === "" ? null : Number(trimmed);
		if (value != null && !Number.isFinite(value)) {
			setStatus("failed");
			return;
		}
		setBusy(true);
		setStatus("idle");
		try {
			setSettings(await updateSpamThreshold(value));
			setStatus("saved");
		} catch {
			setStatus("failed");
		} finally {
			setBusy(false);
		}
	}

	async function add() {
		const value = address.trim();
		if (!value) return;
		setBusy(true);
		setStatus("idle");
		try {
			await updateSpamList(value, target);
			setSettings(await fetchSpamSettings());
			setAddress("");
			setStatus("saved");
		} catch {
			setStatus("failed");
		} finally {
			setBusy(false);
		}
	}

	async function remove(entry: string) {
		setBusy(true);
		setStatus("idle");
		try {
			await updateSpamList(entry, "none");
			setSettings(await fetchSpamSettings());
			setStatus("saved");
		} catch {
			setStatus("failed");
		} finally {
			setBusy(false);
		}
	}

	if (!settings) return null;

	const lists = [
		{
			kind: "allow",
			labelKey: "settings.spam.allowList",
			entries: settings.allow,
		},
		{
			kind: "block",
			labelKey: "settings.spam.blockList",
			entries: settings.block,
		},
	] as const;

	return (
		<div className="space-y-8">
			<section className="space-y-3">
				<div>
					<h3 className="text-sm font-medium text-neutral-900">
						{t("settings.spam.threshold")}
					</h3>
					<p className="mt-1 text-xs text-neutral-500">
						{t("settings.spam.thresholdHint", {
							default: settings.defaultThreshold,
						})}
					</p>
				</div>
				<div className="flex items-center gap-3">
					<Input
						type="number"
						step="0.5"
						min="0"
						value={threshold}
						onChange={(event) => setThreshold(event.target.value)}
						placeholder={String(settings.defaultThreshold)}
						className="max-w-32"
						aria-label={t("settings.spam.threshold")}
					/>
					<Button
						type="button"
						onClick={() => void saveThreshold()}
						disabled={busy}
					>
						{t("settings.spam.save")}
					</Button>
				</div>
			</section>

			<section className="space-y-3">
				<div className="flex gap-2">
					<Button
						type="button"
						variant={target === "block" ? "default" : "outline"}
						size="sm"
						onClick={() => setTarget("block")}
					>
						{t("settings.spam.blockList")}
					</Button>
					<Button
						type="button"
						variant={target === "allow" ? "default" : "outline"}
						size="sm"
						onClick={() => setTarget("allow")}
					>
						{t("settings.spam.allowList")}
					</Button>
				</div>
				<div className="flex items-center gap-3">
					<Input
						type="email"
						value={address}
						onChange={(event) => setAddress(event.target.value)}
						placeholder={t("settings.spam.addressPlaceholder")}
						autoComplete="off"
						aria-label={t("settings.spam.addressPlaceholder")}
					/>
					<Button
						type="button"
						onClick={() => void add()}
						disabled={busy || !address.trim()}
					>
						{t("settings.spam.add")}
					</Button>
				</div>
				<div className="grid gap-4 sm:grid-cols-2">
					{lists.map((list) => (
						<div key={list.kind} className="space-y-2">
							<h3 className="text-sm font-medium text-neutral-900">
								{t(list.labelKey)}
							</h3>
							{list.entries.length === 0 ? (
								<p className="text-xs text-neutral-500">
									{t("settings.spam.empty")}
								</p>
							) : (
								<ul className="space-y-1">
									{list.entries.map((entry) => (
										<li
											key={entry}
											className="flex items-center justify-between gap-2 text-sm"
										>
											<span className="truncate">{entry}</span>
											<Button
												type="button"
												variant="ghost"
												size="roundedSM"
												onClick={() => void remove(entry)}
												disabled={busy}
												aria-label={t("settings.spam.remove")}
											>
												x
											</Button>
										</li>
									))}
								</ul>
							)}
						</div>
					))}
				</div>
			</section>

			{status === "saved" && (
				<p className="text-sm text-neutral-500">{t("settings.spam.saved")}</p>
			)}
			{status === "failed" && (
				<p className="text-sm text-red-600">{t("settings.spam.saveFailed")}</p>
			)}
		</div>
	);
}
