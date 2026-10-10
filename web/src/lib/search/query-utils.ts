import { dateFromZonedFields, normalizeTimeZone } from "@/lib/time/utils";
import type { ParsedSearchQuery } from "./types";

/**
 * Gmail-style search grammar shared by the search box and the API:
 *
 *   from:maya to:"sam okoro" subject:invoice has:attachment is:unread
 *   is:starred after:2026-09-01 before:2026-09-30 "exact phrase" -excluded word
 *
 * The older `title:` and bare `:unread` / `:read` forms keep working.
 */
const OPERATOR_RE =
	/(?:^|\s)(from|to|subject|title|has|is|after|before|newer|older):(?:"([^"]*)"|(\S+))/gi;

export function parseSearchQuery(
	raw: string,
	timeZone = "UTC",
): ParsedSearchQuery {
	const parsed: ParsedSearchQuery = { text: "", needsFullText: false };
	let rest = raw ?? "";

	rest = rest.replace(
		OPERATOR_RE,
		(
			_match,
			key: string,
			quoted: string | undefined,
			bare: string | undefined,
		) => {
			const value = (quoted ?? bare ?? "").trim();
			switch (key.toLowerCase()) {
				case "from":
					parsed.from = value;
					break;
				case "to":
					parsed.to = value;
					break;
				case "subject":
				case "title":
					parsed.subject = value;
					break;
				case "has":
					if (/^attachments?$/i.test(value)) parsed.hasAttachment = true;
					break;
				case "is":
					if (/^unread$/i.test(value)) parsed.read = "unread";
					else if (/^read$/i.test(value)) parsed.read = "read";
					else if (/^starred$/i.test(value)) parsed.starred = true;
					break;
				case "after":
				case "newer":
					parsed.after = parseDate(value, timeZone) ?? parsed.after;
					break;
				case "before":
				case "older":
					parsed.before = parseDate(value, timeZone) ?? parsed.before;
					break;
			}
			return " ";
		},
	);

	if (/(^|\s):unread(?=\s|$)/i.test(rest)) {
		parsed.read = "unread";
		rest = rest.replace(/(^|\s):unread(?=\s|$)/gi, " ");
	} else if (/(^|\s):read(?=\s|$)/i.test(rest)) {
		parsed.read = "read";
		rest = rest.replace(/(^|\s):read(?=\s|$)/gi, " ");
	}

	parsed.text = rest.replace(/\s+/g, " ").trim();
	parsed.needsFullText = !!(
		parsed.text ||
		parsed.from ||
		parsed.to ||
		parsed.subject
	);
	return parsed;
}

function parseDate(value: string, timeZone: string): Date | undefined {
	const match = value.match(/^(\d{4})[-/](\d{1,2})[-/](\d{1,2})$/);
	if (!match) return undefined;
	const date = new Date(0);
	date.setUTCFullYear(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
	if (
		date.getUTCFullYear() !== Number(match[1]) ||
		date.getUTCMonth() !== Number(match[2]) - 1 ||
		date.getUTCDate() !== Number(match[3])
	)
		return undefined;
	return dateFromZonedFields(date, normalizeTimeZone(timeZone));
}
