import { describe, expect, test } from "bun:test";
import {
	attachmentPreviewUrl,
	inlineAttachmentUrls,
	resolveInlineAttachment,
} from "@/lib/email/inline-attachments";

const messageId = "01a129b9-c82c-7239-8404-ed908b211b2a";

function attachment(overrides: {
	contentId?: string | null;
	filename?: string;
	id?: string;
}) {
	return {
		contentId: overrides.contentId ?? null,
		filename: overrides.filename ?? "attachment",
		id: overrides.id ?? "0",
	};
}

describe("inline attachment references", () => {
	test("resolves the Content-ID a body writes", () => {
		const urls = inlineAttachmentUrls(messageId, [
			attachment({
				contentId: "image001.png@01D9ABCD.5F2C4E80",
				filename: "image001.png",
			}),
		]);
		const url = attachmentPreviewUrl(messageId, "0");
		expect(
			resolveInlineAttachment("cid:image001.png@01D9ABCD.5F2C4E80", urls),
		).toBe(url);
		expect(
			resolveInlineAttachment("CID:image001.png@01D9ABCD.5F2C4E80", urls),
		).toBe(url);
		expect(
			resolveInlineAttachment("cid:<image001.png@01D9ABCD.5F2C4E80>", urls),
		).toBe(url);
	});

	test("resolves a cid written as the filename, or the local part alone", () => {
		const urls = inlineAttachmentUrls(messageId, [
			attachment({
				contentId: "image001.png@01D9ABCD.5F2C4E80",
				filename: "image001.png",
			}),
		]);
		const url = attachmentPreviewUrl(messageId, "0");
		for (const reference of [
			"cid:image001.png",
			"cid:image001.png@01D9ABCD.5F2C4E80",
			"cid:IMAGE001.PNG@01d9abcd.5f2c4e80",
			"image001.png",
		])
			expect(resolveInlineAttachment(reference, urls)).toBe(url);
	});

	test("resolves a percent-encoded cid", () => {
		const urls = inlineAttachmentUrls(messageId, [
			attachment({
				contentId: "logo image@example.test",
				filename: "logo.png",
			}),
		]);
		expect(resolveInlineAttachment("cid:logo%20image@example.test", urls)).toBe(
			attachmentPreviewUrl(messageId, "0"),
		);
	});

	test("gives each part its own URL when names collide", () => {
		const urls = inlineAttachmentUrls(messageId, [
			attachment({
				contentId: "logo@example.test",
				filename: "logo.png",
				id: "0",
			}),
			attachment({
				contentId: "banner@example.test",
				filename: "logo.png",
				id: "1",
			}),
		]);
		expect(resolveInlineAttachment("cid:logo@example.test", urls)).toBe(
			attachmentPreviewUrl(messageId, "0"),
		);
		expect(resolveInlineAttachment("cid:banner@example.test", urls)).toBe(
			attachmentPreviewUrl(messageId, "1"),
		);
		// The earlier part keeps the shared filename.
		expect(resolveInlineAttachment("cid:logo.png", urls)).toBe(
			attachmentPreviewUrl(messageId, "0"),
		);
	});

	test("leaves remote, data and unknown references alone", () => {
		const urls = inlineAttachmentUrls(messageId, [
			attachment({ contentId: "logo@example.test", filename: "logo.png" }),
		]);
		for (const reference of [
			"https://example.test/logo.png",
			"http://example.test/logo.png",
			"data:image/png;base64,aGk=",
			"cid:missing@example.test",
			"#section",
			"/assets/logo.png",
			"/api/messages/other/attachments/0?preview=1",
		])
			expect(resolveInlineAttachment(reference, urls)).toBe(reference);
	});

	test("maps only names a part answers to", () => {
		const urls = inlineAttachmentUrls(messageId, [
			attachment({ contentId: null, filename: "report.pdf", id: "1" }),
		]);
		// A cid-only reference cannot match an attachment that names no part.
		expect(resolveInlineAttachment("cid:report.pdf", urls)).toBe(
			attachmentPreviewUrl(messageId, "1"),
		);
		expect(inlineAttachmentUrls(messageId, []).size).toBe(0);
		expect(
			resolveInlineAttachment("cid:anything@example.test", new Map()),
		).toBe("cid:anything@example.test");
	});
});
