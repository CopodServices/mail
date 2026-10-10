"use client";

import { apiUrl } from "@/lib/api/base";
import { csrfHeaders, redirectToLogin } from "@/lib/api/session-utils";
import { getUserTimeZone } from "@/lib/time/utils";

/**
 * The one transport for the mail API: everything the client sends goes through
 * apiFetch. The endpoint-shaped helpers built on it live in ./endpoints.ts.
 *
 * The browser authenticates with the httpOnly session cookie the API sets at
 * sign-in, so every call carries credentials: "same-origin" — the bundle is
 * served from the API's own origin (the container's nginx proxies /api/*).
 * Writes also carry the double-submit CSRF header. A 401 from any call means
 * the session expired or was cleared, so the client returns to the login page.
 * Every call goes through here, so that policy lives in one place.
 */
type ApiFetchOptions = RequestInit & {
	/**
	 * Default true: a 401 means the session is gone, so the browser returns to
	 * the login page. Pass false where a 401 is a normal outcome, for example a
	 * probe that only needs to know the call failed.
	 */
	redirectOnUnauthorized?: boolean;
};

function apiHeaders(headers?: HeadersInit): Headers {
	const next = new Headers(headers);
	if (typeof window !== "undefined" && !next.has("X-Time-Zone")) {
		next.set("X-Time-Zone", getUserTimeZone());
	}
	return next;
}

export async function apiFetch(
	input: RequestInfo | URL,
	init: ApiFetchOptions = {},
): Promise<Response> {
	const { headers, redirectOnUnauthorized = true, ...requestInit } = init;
	const url = typeof input === "string" ? apiUrl(input) : input;
	const method = requestInit.method ?? "GET";
	const next = apiHeaders(headers);
	for (const [key, value] of Object.entries(csrfHeaders(method))) {
		if (!next.has(key)) next.set(key, value);
	}
	const response = await fetch(url, {
		...requestInit,
		credentials: "same-origin",
		headers: next,
	});
	if (response.status === 401 && redirectOnUnauthorized) {
		redirectToLogin(
			typeof window === "undefined"
				? null
				: window.location.pathname + window.location.search,
		);
	}
	return response;
}
