/**
 * Inline parts and the references that name them.
 *
 * A body embeds a picture with a cid: reference, and the part it names is an
 * ordinary MIME part of the same message. Senders disagree on what that
 * reference contains: Outlook writes the Content-ID (usually with an @ host),
 * Apple Mail writes the filename, and some clients write the filename without
 * the scheme at all. Each spelling is mapped here to the one URL that serves
 * the part, so the reader can draw it in place instead of an empty box.
 */

export type InlineAttachment = {
	contentId: string | null;
	filename: string;
	id: string;
};

/** The URL that serves one part's bytes for display inside the body. */
export function attachmentPreviewUrl(
	messageId: string,
	attachmentId: string,
): string {
	return `/api/messages/${messageId}/attachments/${attachmentId}?preview=1`;
}

/** A name without the header syntax around it: angle brackets and whitespace. */
function normalize(value: string | null | undefined): string | null {
	const trimmed = value
		?.trim()
		.replace(/^<+|>+$/g, "")
		.trim();
	return trimmed ? trimmed : null;
}

/**
 * The spellings one name is written in. Some senders percent-encode a cid: and
 * Content-IDs are compared loosely, so the lowercased form is offered too. The
 * first spelling of the highest priority wins when two parts share a name.
 */
function spellings(name: string | null | undefined): string[] {
	const value = normalize(name);
	if (!value) return [];
	const found = [value, value.toLowerCase()];
	try {
		found.push(decodeURIComponent(value));
	} catch {
		/* A name that is not percent-encoded is already covered. */
	}
	return [...new Set(found)];
}

/**
 * Every name a body may use for one part, best match first: the Content-ID, the
 * local part before its @ host, the filename, then the filename without its
 * extension.
 */
function claimGroups(attachment: InlineAttachment): Array<{
	priority: number;
	names: string[];
}> {
	const contentId = normalize(attachment.contentId);
	const filename = normalize(attachment.filename);
	return [
		{ priority: 0, names: spellings(contentId) },
		{
			priority: 1,
			names: spellings(contentId ? contentId.split("@")[0] : null),
		},
		{ priority: 2, names: spellings(filename) },
		{
			priority: 3,
			names: spellings(filename ? filename.replace(/\.[^./]+$/, "") : null),
		},
	];
}

/**
 * Maps every name a body may use for an inline part to the URL that serves it.
 * Names are claimed in priority order, so one part's Content-ID is never
 * shadowed by an earlier part's filename.
 */
export function inlineAttachmentUrls(
	messageId: string,
	attachments: InlineAttachment[],
): Map<string, string> {
	const urls = new Map<string, string>();
	const claims: Array<{ priority: number; name: string; url: string }> = [];
	for (const attachment of attachments) {
		const url = attachmentPreviewUrl(messageId, attachment.id);
		for (const group of claimGroups(attachment))
			for (const name of group.names)
				claims.push({ priority: group.priority, name, url });
	}
	claims.sort((left, right) => left.priority - right.priority);
	for (const claim of claims)
		if (!urls.has(claim.name)) urls.set(claim.name, claim.url);
	return urls;
}

/** A reference a body writes without a scheme: a bare part name, not a URL. */
function isBareName(reference: string): boolean {
	if (!reference || reference.startsWith("#") || reference.startsWith("/"))
		return false;
	// Anything with a scheme (http:, data:, blob:, mailto:, ...) is an address.
	if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(reference)) return false;
	return !reference.includes("/") && !/\s/.test(reference);
}

/** The percent-decoded reference, when it decodes. */
function decode(reference: string): string | null {
	try {
		return decodeURIComponent(reference);
	} catch {
		return null;
	}
}

/**
 * The preview URL for one body reference, or the reference unchanged when no
 * part answers to it. Remote and data URLs are left exactly as they are.
 */
export function resolveInlineAttachment(
	reference: string,
	urls: Map<string, string>,
): string {
	if (!urls.size) return reference;
	const value = reference.trim();
	const isCid = /^cid:/i.test(value);
	if (!isCid && !isBareName(value)) return reference;
	const name = isCid ? value.slice(4) : value;
	const decoded = decode(name);
	// The reference is the name itself, its percent-decoded form, or either
	// without the angle brackets a Content-ID header writes around it.
	const candidates = new Set(
		[name, decoded, normalize(name), decoded && normalize(decoded)].filter(
			(candidate): candidate is string => !!candidate,
		),
	);
	for (const candidate of candidates) {
		const hit = urls.get(candidate) ?? urls.get(candidate.toLowerCase());
		if (hit) return hit;
	}
	return reference;
}
