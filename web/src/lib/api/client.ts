"use client";

import { apiUrl } from "@/lib/mail-api/base";
import { getUserTimeZone } from "@/lib/time/utils";

/**
 * Thin fetch wrapper for the mail API. The browser is a plain web client: it
 * holds no credentials, and anything secret (an API key, for instance) stays
 * with the service that fronts the mail API, not in this bundle. Every call
 * goes through here so that stays in one place.
 */
type ApiFetchOptions = RequestInit;

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
	const { headers, ...requestInit } = init;
	const url = typeof input === "string" ? apiUrl(input) : input;
	return fetch(url, { ...requestInit, headers: apiHeaders(headers) });
}
