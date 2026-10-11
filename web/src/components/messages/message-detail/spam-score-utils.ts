/**
 * The API stores the engine's tag summary as a single string, e.g.
 * `URL_SHORTENER (1.50), IPREV_FAIL (1.00)`. Each entry is a detector name and
 * the signed weight it contributed; negative weights are allow-list rules that
 * pulled the message back toward the inbox. The reader splits that summary into
 * rows it can render as chips, keeps the order stable, and drops anything it
 * cannot parse rather than showing a raw fragment.
 */

export type SpamSignal = {
	/** The detector or rule tag, e.g. `URL_SHORTENER`. */
	name: string;
	/** The signed weight the tag added; negative for allow-list rules. */
	weight: number;
	/** `true` when the tag pushed toward junk, `false` when it pulled back. */
	positive: boolean;
};

/** A single tag and its weight, e.g. `NAME (1.50)`. */
const SIGNAL_RE = /([^,()]+?)\s*\(\s*([+-]?\d+(?:\.\d+)?)\s*\)/g;

/** Splits the stored tag summary into per-tag rows, preserving order. */
export function parseSpamSignals(signals?: string | null): SpamSignal[] {
	if (!signals) return [];
	const trimmed = signals.trim();
	if (!trimmed || trimmed === "-") return [];
	const rows: SpamSignal[] = [];
	SIGNAL_RE.lastIndex = 0;
	for (const match of trimmed.matchAll(SIGNAL_RE)) {
		const name = match[1]?.trim();
		const weight = Number.parseFloat(match[2] ?? "");
		if (!name || !Number.isFinite(weight)) continue;
		rows.push({ name, weight, positive: weight >= 0 });
	}
	return rows;
}

/**
 * The largest tag weight in a set of signals, or `1` when there is none. The
 * chip meters are scaled against this so the heaviest detector always reads as
 * a full bar and the rest stay proportional.
 */
export function maxSignalMagnitude(signals: SpamSignal[]): number {
	let max = 1;
	for (const signal of signals) {
		max = Math.max(max, Math.abs(signal.weight));
	}
	return max;
}

/** The upper end of the score gauge: the default junk threshold (5) doubled. */
export const SPAM_SCORE_CEILING = 10;

/** A number in `0..100` for a score scaled against {@link SPAM_SCORE_CEILING}. */
export function scorePercent(score: number): number {
	return Math.max(0, Math.min(100, (score / SPAM_SCORE_CEILING) * 100));
}
