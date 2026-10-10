import { getEmailDisplayName } from "@/lib/email/address";

export function getDisplayNameForAddress(
	address: string,
	contactName?: string | null,
): string {
	return contactName?.trim() || getEmailDisplayName(address);
}
