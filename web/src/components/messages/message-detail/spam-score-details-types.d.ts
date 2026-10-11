export type SpamVerdict = "inbox" | "suspicious" | "spam";

export type SpamScoreDetailsProps = {
	score?: number | null;
	verdict?: SpamVerdict | null;
	signals?: string | null;
};
