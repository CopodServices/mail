/**
 * Where the browser sends API calls.
 *
 * Empty by default: the client calls the same-origin /api/* paths and the
 * container's reverse proxy forwards them to the mail API. Set
 * NEXT_PUBLIC_MAIL_API_URL to an absolute origin to talk to the API directly
 * (for example a local dev API at http://localhost:8080).
 */
const raw = process.env.NEXT_PUBLIC_MAIL_API_URL ?? "";

export const MAIL_API_BASE = raw.replace(/\/+$/, "");

export function apiUrl(path: string): string {
	return path.startsWith("/") ? MAIL_API_BASE + path : path;
}
