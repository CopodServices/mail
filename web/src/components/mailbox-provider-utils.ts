import { apiFetch } from "@/lib/api/client";
import type { MailboxOption } from "./mailbox-provider";

let mailboxesCache: MailboxOption[] | null = null;
let mailboxesRequest: Promise<MailboxOption[]> | null = null;
let cacheGeneration = 0;
export const SELECTED_MAILBOX_STORAGE_KEY = "selected-mailbox-id";

export async function fetchMailboxOptions(
	force = false,
): Promise<MailboxOption[]> {
	if (!force && mailboxesCache) return mailboxesCache;
	if (!force && mailboxesRequest) return mailboxesRequest;

	const requestGeneration = cacheGeneration;
	mailboxesRequest = apiFetch("/api/mailboxes")
		.then((res) => res.json())
		.then((data) => {
			const items = (
				(
					data as {
						data?: Array<{
							id: string;
							address: string;
							active: boolean;
							signature: string | null;
							display_name: string | null;
							forward_to: string | null;
						}>;
					}
				).data ?? []
			).map((mailbox) => {
				const [localPart, hostname = ""] = mailbox.address.split("@");
				return {
					id: mailbox.id,
					domainId: "",
					localPart,
					hostname,
					// The stored name rides in the From header; the local part is
					// only the fallback for a mailbox that has not set one.
					displayName: mailbox.display_name ?? localPart,
					signature: mailbox.signature ?? null,
					forwardTo: mailbox.forward_to ?? null,
					hasAvatar: false,
					type: "personal" as const,
					isPrimary: false,
					senderAddresses: [mailbox.address],
				};
			});
			if (requestGeneration === cacheGeneration) {
				mailboxesCache = items;
			}
			return items;
		})
		.finally(() => {
			if (requestGeneration === cacheGeneration) {
				mailboxesRequest = null;
			}
		});

	return mailboxesRequest;
}
