/**
 * The composer accepts a colleague by mailbox name alone: the sender's own
 * domain is filled in on commit, on loading a stored draft and again on send.
 * These pin the rules around that completion and the chip label it feeds,
 * because the API only ever accepts a full address.
 */

import { describe, expect, test } from "bun:test";
import {
	completeRecipientEntry,
	getRecipientLabel,
	headerToRecipients,
	isValidRecipient,
	parseRecipientEntries,
} from "@/components/compose/recipient-utils";

describe("recipient completion", () => {
	test("a bare mailbox name takes the sender's domain", () => {
		expect(completeRecipientEntry("imsamuel160", "mandalakarya.test")).toBe(
			"imsamuel160@mandalakarya.test",
		);
		expect(parseRecipientEntries("maya", [], "example.test")).toEqual([
			"maya@example.test",
		]);
	});

	test("an address that carries its own domain is left alone", () => {
		expect(completeRecipientEntry("maya@other.test", "example.test")).toBe(
			"maya@other.test",
		);
	});

	test("a display name rides along with the completed address", () => {
		expect(
			parseRecipientEntries('"Maya Chen" <maya>', [], "example.test"),
		).toEqual(['"Maya Chen" <maya@example.test>']);
	});

	test("without a domain nothing is invented", () => {
		expect(completeRecipientEntry("maya", "")).toBe("maya");
		expect(parseRecipientEntries("maya", [])).toEqual(["maya"]);
		expect(completeRecipientEntry("maya smith", "example.test")).toBe(
			"maya smith",
		);
	});

	test("completion still deduplicates by bare address", () => {
		expect(
			parseRecipientEntries("maya", ["maya@example.test"], "example.test"),
		).toEqual([]);
		expect(
			parseRecipientEntries("maya@example.test, maya", [], "example.test"),
		).toEqual(["maya@example.test"]);
	});

	test("a header of bare names resolves as a list", () => {
		expect(headerToRecipients("maya, sam@other.test", "example.test")).toEqual([
			"maya@example.test",
			"sam@other.test",
		]);
	});

	test("a completed address is valid where the bare name was not", () => {
		expect(isValidRecipient("imsamuel160")).toBe(false);
		expect(isValidRecipient("imsamuel160@mandalakarya.test")).toBe(true);
	});
});

describe("recipient chip labels", () => {
	test("the sender's own domain drops to the mailbox name", () => {
		expect(
			getRecipientLabel("imsamuel160@mandalakarya.test", "Mandalakarya.test"),
		).toBe("imsamuel160");
	});

	test("another domain keeps the whole address", () => {
		expect(getRecipientLabel("maya@other.test", "example.test")).toBe(
			"maya@other.test",
		);
		expect(getRecipientLabel("maya@example.test")).toBe("maya@example.test");
	});

	test("a display name still wins", () => {
		expect(
			getRecipientLabel('"Maya Chen" <maya@example.test>', "example.test"),
		).toBe("Maya Chen");
	});
});
