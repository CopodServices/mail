"use client";

import { useLanguage } from "@/components/language-provider";

type SpamScoreDetailsProps = {
	score?: number | null;
	verdict?: "inbox" | "suspicious" | "spam" | null;
	signals?: string | null;
};

/**
 * The spam verdict for an open message: the total score, the verdict band and
 * the tag summary the engine recorded, when it scored the message.
 */
export function SpamScoreDetails({
	score,
	verdict,
	signals,
}: SpamScoreDetailsProps) {
	const { t } = useLanguage();
	if (score == null && !signals) return null;
	const label =
		verdict === "spam"
			? t("spam.verdict.spam")
			: verdict === "suspicious"
				? t("spam.verdict.suspicious")
				: t("spam.verdict.inbox");
	return (
		<div className="rounded-2xl bg-white/60 px-4 py-3 text-sm text-neutral-600">
			<div className="flex flex-wrap items-center gap-x-4 gap-y-1">
				<span className="font-medium text-neutral-900">{t("spam.title")}</span>
				{score != null && (
					<span>
						{t("spam.score")}: {score.toFixed(2)}
					</span>
				)}
				<span>
					{t("spam.verdict")}: {label}
				</span>
			</div>
			{signals && (
				<p className="mt-1 break-words text-xs text-neutral-500">
					{t("spam.signals")}: {signals}
				</p>
			)}
		</div>
	);
}
