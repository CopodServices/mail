"use client";

import { apiUrl } from "@/lib/mail-api/base";
import { getUserTimeZone } from "@/lib/time/utils";

/**
 * Thin fetch wrapper for the mail API. There is no session yet: the client
 * authenticates with an API key when one is configured, and external SSO will
 * replace this later. Every call goes through here so that stays in one place.
 */
const API_KEY_STORAGE_KEY = "copod:api-key";

/** Optional key baked in at build time; a value saved in the browser wins. */
const ENV_API_KEY = process.env.NEXT_PUBLIC_MAIL_API_KEY ?? "";

export function getApiKey(): string | null {
	if (typeof window === "undefined") return ENV_API_KEY || null;
	try {
		return (
			window.localStorage.getItem(API_KEY_STORAGE_KEY) ?? (ENV_API_KEY || null)
		);
	} catch {
		return ENV_API_KEY || null;
	}
}

export function setApiKey(key: string | null): void {
	if (typeof window === "undefined") return;
	try {
		if (key) window.localStorage.setItem(API_KEY_STORAGE_KEY, key);
		else window.localStorage.removeItem(API_KEY_STORAGE_KEY);
	} catch {
		// Storage can be unavailable; the key then only lives for this call.
	}
}

export type ApiFetchOptions = RequestInit;

export function apiHeaders(headers?: HeadersInit): Headers {
	const next = new Headers(headers);
	if (typeof window !== "undefined" && !next.has("X-Time-Zone")) {
		next.set("X-Time-Zone", getUserTimeZone());
	}
	const key = getApiKey();
	if (key && !next.has("Authorization")) {
		next.set("Authorization", "Bearer " + key);
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
