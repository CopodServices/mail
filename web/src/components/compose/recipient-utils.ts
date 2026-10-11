import {
	formatEmailAddress,
	getEmailAddress,
	joinEmailAddressList,
	parseEmailAddressParts,
	splitEmailAddressList,
} from "@/lib/email/address";

const EMAIL_RE = /^[^\s@<>"]+@[^\s@<>"]+\.[^\s@<>"]+$/;

export function isValidRecipient(entry: string): boolean {
	return EMAIL_RE.test(getEmailAddress(entry).trim());
}

/** The bare domain an address belongs to, without the leading at-sign. */
function bareDomain(domain: string | null | undefined): string {
	return domain?.trim().replace(/^@/, "") ?? "";
}

/**
 * Complete a mailbox name with the domain the sender writes from, so a
 * colleague can be addressed by name alone (`maya` -> `maya@example.test`).
 * An entry that already carries a domain, a display name only, or anything
 * that is not a single token is left exactly as typed.
 */
export function completeRecipientEntry(
	entry: string,
	domain: string | null | undefined,
): string {
	const trimmed = entry.trim();
	const normalizedDomain = bareDomain(domain);
	if (!trimmed || !normalizedDomain) return trimmed;

	const parts = parseEmailAddressParts(trimmed);
	const address = parts.address.trim();
	if (!address || address.includes("@") || /\s/.test(address)) return trimmed;
	return formatEmailAddress(`${address}@${normalizedDomain}`, parts.name);
}

/** Turn raw typed text into recipient entries, deduplicated by bare address. */
export function parseRecipientEntries(
	value: string,
	existing: string[] = [],
	defaultDomain?: string | null,
): string[] {
	const seen = new Set(
		existing.map((entry) => getEmailAddress(entry).toLowerCase()),
	);
	const result: string[] = [];
	for (const raw of splitEmailAddressList(value)) {
		const entry = completeRecipientEntry(raw, defaultDomain);
		const key = getEmailAddress(entry).toLowerCase();
		if (!key || seen.has(key)) continue;
		seen.add(key);
		result.push(entry);
	}
	return result;
}

export function recipientsToHeader(entries: string[]): string {
	return joinEmailAddressList(entries);
}

export function headerToRecipients(
	value: string | null | undefined,
	defaultDomain?: string | null,
): string[] {
	return parseRecipientEntries(value ?? "", [], defaultDomain);
}

/**
 * Short label for a chip: the display name if there is one, the mailbox name
 * when the address belongs to the sender's own domain, else the address.
 */
export function getRecipientLabel(
	entry: string,
	ownDomain?: string | null,
): string {
	const parts = parseEmailAddressParts(entry);
	if (parts.name && parts.name !== parts.address) return parts.name;

	const address = parts.address;
	const domain = bareDomain(ownDomain).toLowerCase();
	const at = address.lastIndexOf("@");
	if (domain && at > 0 && address.slice(at + 1).toLowerCase() === domain) {
		return address.slice(0, at);
	}
	return address;
}
