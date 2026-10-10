import { apiFetch } from "@/lib/api/client";
import { getEmailAddress } from "@/lib/email/address";
import type { ContactDetailsRecord } from "./contact-details-types";

/** The contacts API answers with its own record, wrapped in a data envelope. */
type ApiContact = {
	email: string;
	display_name: string | null;
	has_avatar: boolean;
	source: string;
	blocked: boolean;
	last_seen_at: string | null;
};

type ApiContactEnvelope = { data?: ApiContact; error?: string };

function toContactDetails(contact: ApiContact): ContactDetailsRecord {
	return {
		email: contact.email,
		displayName: contact.display_name,
		hasAvatar: contact.has_avatar,
		source: contact.source as ContactDetailsRecord["source"],
		blocked: contact.blocked,
		lastSeenAt: contact.last_seen_at,
	};
}

export async function fetchContactDetails(
	mailboxId: string,
	address: string,
): Promise<ContactDetailsRecord> {
	const params = new URLSearchParams({
		mailbox_id: mailboxId,
		address: getEmailAddress(address),
	});
	const response = await apiFetch(`/api/contacts?${params.toString()}`);
	const data = (await response.json()) as ApiContactEnvelope;
	if (!response.ok || !data.data)
		throw new Error(data.error ?? "Unable to load contact");
	return toContactDetails(data.data);
}

export async function updateContactName(
	mailboxId: string,
	address: string,
	displayName: string,
): Promise<ContactDetailsRecord> {
	const response = await apiFetch("/api/contacts", {
		method: "PATCH",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify({
			mailbox_id: mailboxId,
			address: getEmailAddress(address),
			display_name: displayName,
		}),
	});
	const data = (await response.json()) as ApiContactEnvelope;
	if (!response.ok || !data.data)
		throw new Error(data.error ?? "Unable to update contact");
	return toContactDetails(data.data);
}
