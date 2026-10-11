/**
 * The reader shows the engine's tag summary as individual chips, so the split
 * has to hold up for the strings the engine actually stores: `NAME (1.50)`
 * pairs joined by commas, negative allow-list weights, and junk that should be
 * dropped rather than rendered as a raw fragment.
 */

import { describe, expect, test } from "bun:test";
import {
	SPAM_SCORE_CEILING,
	maxSignalMagnitude,
	parseSpamSignals,
	scorePercent,
} from "@/components/messages/message-detail/spam-score-utils";

describe("parseSpamSignals", () => {
	test("splits the stored tag summary into named, weighted rows", () => {
		expect(
			parseSpamSignals(
				"URL_SHORTENER (1.50), IPREV_FAIL (1.00), NO_RECEIVED (1.00), SUBJECT_ALL_CAPS (1.00)",
			),
		).toEqual([
			{ name: "URL_SHORTENER", weight: 1.5, positive: true },
			{ name: "IPREV_FAIL", weight: 1, positive: true },
			{ name: "NO_RECEIVED", weight: 1, positive: true },
			{ name: "SUBJECT_ALL_CAPS", weight: 1, positive: true },
		]);
	});

	test("keeps the sign so allow-list rules read as mitigating", () => {
		expect(parseSpamSignals("ALLOWED_SENDER (-2.50)")).toEqual([
			{ name: "ALLOWED_SENDER", weight: -2.5, positive: false },
		]);
	});

	test("returns nothing for an absent or empty summary", () => {
		expect(parseSpamSignals(null)).toEqual([]);
		expect(parseSpamSignals(undefined)).toEqual([]);
		expect(parseSpamSignals("")).toEqual([]);
		expect(parseSpamSignals("-")).toEqual([]);
	});

	test("drops entries that are not a name and a weight", () => {
		expect(parseSpamSignals("just some words, BAYES_SPAM (2.00)")).toEqual([
			{ name: "BAYES_SPAM", weight: 2, positive: true },
		]);
	});

	test("tolerates whitespace around the name and weight", () => {
		expect(parseSpamSignals("RBL_ZEN   ( 6.00 )")).toEqual([
			{ name: "RBL_ZEN", weight: 6, positive: true },
		]);
	});
});

describe("maxSignalMagnitude", () => {
	test("scales against the heaviest tag, defaulting to one", () => {
		expect(maxSignalMagnitude([])).toBe(1);
		expect(
			maxSignalMagnitude(parseSpamSignals("A (1.50), B (4.00)")),
		).toBe(4);
	});
});

describe("scorePercent", () => {
	test("maps a score onto the gauge and clamps the ends", () => {
		expect(scorePercent(0)).toBe(0);
		expect(scorePercent(SPAM_SCORE_CEILING / 2)).toBe(50);
		expect(scorePercent(SPAM_SCORE_CEILING)).toBe(100);
		expect(scorePercent(SPAM_SCORE_CEILING * 3)).toBe(100);
		expect(scorePercent(-1)).toBe(0);
	});
});
