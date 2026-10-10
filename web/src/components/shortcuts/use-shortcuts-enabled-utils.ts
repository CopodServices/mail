import { apiFetch } from "@/lib/api/client";
import type { ShortcutsSettingsResponse } from "./types";

export async function loadShortcutsEnabled(): Promise<boolean> {
	const response = await apiFetch("/api/settings/shortcuts");
	const data =
		((await response.json()) as { data?: ShortcutsSettingsResponse }).data ??
		{};
	if (!response.ok || typeof data.enabled !== "boolean") {
		throw new Error(
			typeof data.error === "string"
				? data.error
				: "Failed to load shortcut settings",
		);
	}
	return data.enabled;
}

export async function updateShortcutsEnabled(
	enabled: boolean,
): Promise<boolean> {
	const response = await apiFetch("/api/settings/shortcuts", {
		method: "PATCH",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify({ enabled }),
	});
	const data =
		((await response.json()) as { data?: ShortcutsSettingsResponse }).data ??
		{};
	if (!response.ok || typeof data.enabled !== "boolean") {
		throw new Error(
			typeof data.error === "string"
				? data.error
				: "Failed to update shortcut settings",
		);
	}
	return data.enabled;
}
