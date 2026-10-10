/**
 * Session cookie plumbing shared by the API client and the login page.
 *
 * The API sets an httpOnly session cookie plus a readable CSRF cookie; writes
 * carry the CSRF value back in X-CSRF-Token (double submit).
 */
export const SESSION_COOKIE = "cpod_mail_session";
export const CSRF_COOKIE = "cpod_mail_csrf";
export const LOGIN_PATH = "/login";
export const DEFAULT_AFTER_LOGIN = "/inbox";

const SAFE_METHODS = ["GET", "HEAD", "OPTIONS"];

/** Reads one cookie value, defaulting to document.cookie in the browser. */
export function readCookie(name: string, cookie?: string): string | null {
	const source =
		cookie ?? (typeof document === "undefined" ? "" : document.cookie);
	for (const part of source.split(";")) {
		const index = part.indexOf("=");
		if (index === -1) continue;
		if (part.slice(0, index).trim() !== name) continue;
		return decodeURIComponent(part.slice(index + 1).trim());
	}
	return null;
}

/** The CSRF header a cookie-authenticated write needs, or nothing for a read. */
export function csrfHeaders(
	method: string,
	cookie?: string,
): Record<string, string> {
	if (SAFE_METHODS.includes(method.toUpperCase())) return {};
	const token = readCookie(CSRF_COOKIE, cookie);
	return token ? { "X-CSRF-Token": token } : {};
}

/** Only same-origin, path-style return targets are honoured. */
export function safeNext(
	value: string | null | undefined,
	fallback: string = DEFAULT_AFTER_LOGIN,
): string {
	if (value && value.startsWith("/") && !value.startsWith("//")) return value;
	return fallback;
}

export function loginHref(next?: string | null): string {
	const target = safeNext(next, "");
	return target
		? LOGIN_PATH + "?next=" + encodeURIComponent(target)
		: LOGIN_PATH;
}

/** Sends an unauthenticated caller to the login page (no-op on the server). */
export function redirectToLogin(next?: string | null): void {
	if (typeof window === "undefined") return;
	if (window.location.pathname.startsWith(LOGIN_PATH)) return;
	window.location.assign(loginHref(next));
}
