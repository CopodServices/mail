"use client";

import {
	AlarmClock,
	AtSign,
	Ban,
	Globe,
	Link2,
	TriangleAlert,
	Type,
} from "lucide-react";
import { useLanguage } from "@/components/language-provider";
import { cn } from "@/lib/utils";
import type {
	SpamScoreDetailsProps,
	SpamVerdict,
} from "./spam-score-details-types";
import {
	parseSpamSignals,
	type SpamSignal,
	scorePercent,
} from "./spam-score-utils";

type VerdictStyle = {
	/** The rounded badge that names the verdict. */
	badge: string;
	/** The status dot and gauge fill. */
	dot: string;
	/** The colour of the gauge fill. */
	bar: string;
};

const VERDICTS: Record<SpamVerdict, VerdictStyle> = {
	inbox: {
		badge: "bg-emerald-50 text-emerald-700 ring-emerald-600/20",
		dot: "bg-emerald-500",
		bar: "bg-emerald-500",
	},
	suspicious: {
		badge: "bg-amber-50 text-amber-800 ring-amber-600/25",
		dot: "bg-amber-500",
		bar: "bg-amber-500",
	},
	spam: {
		badge: "bg-red-50 text-red-700 ring-red-600/20",
		dot: "bg-red-500",
		bar: "bg-red-500",
	},
};

/** Icons for the tag families the engine emits; anything else gets a generic shield. */
const SIGNAL_ICONS: Array<{ match: RegExp; icon: typeof Link2 }> = [
	{ match: /URL|LINK|SHORTENER/, icon: Link2 },
	{ match: /IPREV|RDNS|HELO|RECEIVED|RELAY|FORGED/, icon: Globe },
	{ match: /CLOCK|HOUR|TIME|DATE/, icon: AlarmClock },
	{ match: /SUBJECT|CAPS|FONT|SHOUT|HEADER/, icon: Type },
	{ match: /SPF|DKIM|DMARC|FROM|SENDER|ENVELOPE|AT_/, icon: AtSign },
	{ match: /BLOCK|BLACKLIST|DNSBL|RBL|QUARANTINE|BAN/, icon: Ban },
	{ match: /BAYES|CLASSIFIER|CONTENT|BODY|TOKEN/, icon: TriangleAlert },
];

function iconFor(name: string): typeof Link2 {
	for (const { match, icon } of SIGNAL_ICONS) {
		if (match.test(name)) return icon;
	}
	return Link2;
}

/**
 * The spam verdict for an open message, as one compact strip above the body:
 * a status dot, the score, the verdict, a slim gauge, and the detector tags
 * that fed the score.
 */
export function SpamScoreDetails({
	score,
	verdict,
	signals,
}: SpamScoreDetailsProps) {
	const { t } = useLanguage();
	const tags = parseSpamSignals(signals);
	if (score == null && tags.length === 0) return null;

	const band = VERDICTS[verdict ?? "inbox"];
	const label =
		verdict === "spam"
			? t("spam.verdict.spam")
			: verdict === "suspicious"
				? t("spam.verdict.suspicious")
				: t("spam.verdict.inbox");
	const fill = score == null ? 0 : scorePercent(score);

	return (
		<div className="rounded-lg border border-neutral-200/80 bg-white/70 px-3 py-2 text-neutral-600">
			<div className="flex items-center gap-2 text-xs">
				<span
					className={cn("size-2 shrink-0 rounded-full", band.dot)}
					aria-hidden="true"
				/>
				<span className="min-w-0 truncate text-neutral-500">
					{t("spam.title")}
				</span>
				{score != null && (
					<span className="shrink-0 font-semibold tabular-nums text-neutral-900">
						{score.toFixed(2)}
					</span>
				)}
				<span
					className={cn(
						"shrink-0 rounded-full px-1.5 py-px font-medium ring-1 ring-inset",
						band.badge,
					)}
				>
					{label}
				</span>
				{score != null && (
					<div
						className="ml-auto h-1 min-w-10 flex-1 max-w-32 overflow-hidden rounded-full bg-neutral-200/70"
						title={t("settings.spam.threshold")}
					>
						<div
							className={cn("h-full rounded-full", band.bar)}
							style={{ width: `${fill}%` }}
						/>
					</div>
				)}
			</div>

			{tags.length > 0 && (
				<div
					className="mt-1.5 flex flex-wrap gap-1"
					aria-label={t("spam.contributions")}
				>
					{tags.map((tag) => (
						<SignalChip key={`${tag.name}-${tag.weight}`} signal={tag} />
					))}
				</div>
			)}
		</div>
	);
}

/** One detector tag as a chip, coloured by whether it pushed toward junk. */
function SignalChip({ signal }: { signal: SpamSignal }) {
	const Icon = iconFor(signal.name);
	const chip = signal.positive
		? "bg-red-50 text-red-700"
		: "bg-emerald-50 text-emerald-700";
	const value = `${signal.weight >= 0 ? "+" : "\u2212"}${Math.abs(signal.weight).toFixed(2)}`;
	return (
		<span
			title={`${signal.name} (${signal.weight.toFixed(2)})`}
			className={cn(
				"inline-flex items-center gap-1 rounded px-1.5 py-px text-[11px] font-medium",
				chip,
			)}
		>
			<Icon className="size-2.5 shrink-0 opacity-70" aria-hidden="true" />
			<span className="uppercase tracking-tight">
				{signal.name.replace(/_/g, " ").toLowerCase()}
			</span>
			<span className="tabular-nums opacity-80">{value}</span>
		</span>
	);
}
