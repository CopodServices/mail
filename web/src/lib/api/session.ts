import { apiUrl } from "@/lib/api/base";
import { csrfHeaders, notifySessionChanged } from "./session-utils";
import type { SessionMailbox } from "./types";

/**
 * Session calls that must not bounce on a 401: signing in and checking who is
 * signed in both treat a failed call as "not signed in" rather than redirecting.
 */

type Envelope = { data?: SessionMailbox | null };

export async function signIn(
	address: string,
	password: string,
): Promise<SessionMailbox> {
	const response = await fetch(apiUrl("/api/session"), {
		method: "POST",
		credentials: "same-origin",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify({ address, password }),
	});
	if (!response.ok) {
		throw new Error(
			response.status === 401
				? "session.error.invalid"
				: "session.error.generic",
		);
	}
	const body = (await response.json()) as Envelope;
	if (!body.data) throw new Error("session.error.generic");
	// The API has set the session and CSRF cookies; let the realtime socket know
	// it can authenticate now.
	notifySessionChanged();
	return body.data;
}

/** The signed-in mailbox, or null when there is no valid session. */
export async function fetchSession(): Promise<SessionMailbox | null> {
	try {
		const response = await fetch(apiUrl("/api/session"), {
			credentials: "same-origin",
			cache: "no-store",
		});
		if (!response.ok) return null;
		const body = (await response.json()) as Envelope;
		return body.data ?? null;
	} catch {
		return null;
	}
}

export async function signOut(): Promise<void> {
	try {
		await fetch(apiUrl("/api/session"), {
			method: "DELETE",
			credentials: "same-origin",
			headers: csrfHeaders("DELETE"),
		});
	} catch {
		// The client clears its own state regardless.
	}
	notifySessionChanged();
}
