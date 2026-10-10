/** Byte formatting and progress math for the sidebar storage meter. */

/** Human-readable size in binary units, e.g. 1536 -> "1.5 KB". */
export function formatStorageBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
	const units = ["B", "KB", "MB", "GB", "TB", "PB"];
	let value = bytes;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	const decimals = unit === 0 || value >= 100 ? 0 : 1;
	return `${value.toFixed(decimals)} ${units[unit]}`;
}

/** Used percentage rounded and clamped to 0..100, or null when unlimited. */
export function storagePercent(
	usedBytes: number,
	quotaBytes: number,
): number | null {
	if (quotaBytes <= 0) return null;
	const percent = (usedBytes / quotaBytes) * 100;
	return Math.max(0, Math.min(100, Math.round(percent)));
}

/** The "used / quota" (or "used · Unlimited") line shown in the footer. */
export function storageLabel(
	usedBytes: number,
	quotaBytes: number,
	unlimitedLabel: string,
): string {
	const used = formatStorageBytes(usedBytes);
	if (quotaBytes <= 0) return `${used} · ${unlimitedLabel}`;
	return `${used} / ${formatStorageBytes(quotaBytes)}`;
}
