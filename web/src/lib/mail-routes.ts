/**
 * Client-side routes for the mail client.
 *
 * The bundle is a static export with no server routing, so message and folder
 * selection travels in the query string instead of a path segment. Every link
 * builder goes through these helpers.
 */

export function messageHref(base: string, messageId: string): string {
	const separator = base.includes("?") ? "&" : "?";
	return base + separator + "message=" + encodeURIComponent(messageId);
}

export function folderListHref(folderId: string): string {
	return "/folders?folder=" + encodeURIComponent(folderId);
}
