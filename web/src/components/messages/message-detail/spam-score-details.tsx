"use client";

import { useLanguage } from "@/components/language-provider";
import { cn } from "@/lib/utils";
import {
	AlarmClock,
	AtSign,
	Ban,
	Globe,
	Link2,
	Shield,
	ShieldAlert,
	ShieldCheck,
	ShieldX,
	TriangleAlert,
	Type,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { SpamScoreDetailsProps, SpamVerdict } from "./spam-score-details-types";
import {
	SPAM_SCORE_CEILING,
	parseSpamSignals,
	scorePercent,
	type SpamSignal,
} from "./spam-score-utils";

type VerdictStyle = {
	icon: LucideIcon;
	/** The rounded badge that names the verdict. */
	badge: string;
	/** The shield and gauge tint. */
	accent: string;
	/** The colour of the gauge fill. */
	bar: string;
};

const VERDICTS: Record<SpamVerdict, VerdictStyle> = {
	inbox: {
		icon: ShieldCheck,
		badge: "bg-emerald-50 text-emerald-700 ring-emerald-600/20",
		accent: "text-emerald-600",
		bar: "bg-emerald-500",
	},
	suspicious: {
		icon: ShieldAlert,
		badge: "bg-amber-50 text-amber-800 ring-amber-600/25",
		accent: "text-amber-600",
		bar: "bg-amber-500",
	},
	spam: {
		icon: ShieldX,
		badge: "bg-red-50 text-red-700 ring-red-600/20",
		accent: "text-red-600",
		bar: "bg-red-500",
	},
};

/** Icons for the tag families the engine emits; anything else gets a generic shield. */
const SIGNAL_ICONS: Array<{ match: RegExp; icon: LucideIcon }> = [
	{ match: /URL|LINK|SHORTENER/, icon: Link2 },
	{ match: /IPREV|RDNS|HELO|RECEIVED|RELAY|FORGED/, icon: Globe },
	{ match: /CLOCK|HOUR|TIME|DATE/, icon: AlarmClock },
	{ match: /SUBJECT|CAPS|FONT|SHOUT|HEADER/, icon: Type },
	{ match: /SPF|DKIM|DMARC|FROM|SENDER|ENVELOPE|AT_/, icon: AtSign },
	{ match: /BLOCK|BLACKLIST|DNSBL|RBL|QUARANTINE|BAN/, icon: Ban },
	{ match: /BAYES|CLASSIFIER|CONTENT|BODY|TOKEN/, icon: TriangleAlert },
];

function iconFor(name: string): LucideIcon {
	for (const { match, icon } of SIGNAL_ICONS) {
		if (match.test(name)) return icon;
	}
	return Shield;
}

/**
 * The spam verdict for an open message, shown as a compact panel above the
 * body: the score on its gauge, the verdict band the engine picked, and every
 * detector tag that fed the score as a chip carrying its weight.
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
	const VerdictIcon = band.icon;
	const label =
		verdict === "spam"
			? t("spam.verdict.spam")
			: verdict === "suspicious"
				? t("spam.verdict.suspicious")
				: t("spam.verdict.inbox");
	const marker = (5 / SPAM_SCORE_CEILING) * 100;
	const fill = score == null ? 0 : scorePercent(score);

	return (
		<section
			aria-label={t("spam.title")}
			className="rounded-2xl border border-neutral-200/80 bg-white/70 px-4 py-3.5 text-sm text-neutral-600 shadow-sm"
		>
			<div className="flex items-center justify-between gap-3">
				<div className="flex min-w-0 items-center gap-2 font-medium text-neutral-900">
					<span
						className={cn(
							"flex size-6 shrink-0 items-center justify-center rounded-full bg-neutral-100",
							band.accent,
						)}
						aria-hidden="true"
					>
						<Shield className="size-3.5" />
					</span>
					<span className="truncate">{t("spam.title")}</span>
				</div>
				<span
					className={cn(
						"inline-flex shrink-0 items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ring-1 ring-inset",
						band.badge,
					)}
				>
					<VerdictIcon className="size-3.5" aria-hidden="true" />
					{label}
				</span>
			</div>

			{score != null && (
				<div className="mt-3">
					<div className="flex items-baseline gap-2">
						<span className="text-xs text-neutral-500">
							{t("spam.score")}
						</span>
						<span className="text-xl font-semibold leading-none tabular-nums text-neutral-900">
							{score.toFixed(2)}
						</span>
						<span className="text-xs text-neutral-400">
							/ {SPAM_SCORE_CEILING}
						</span>
					</div>
					<div className="relative mt-2 h-1.5 w-full overflow-hidden rounded-full bg-neutral-200/70">
						<div
							className={cn(
								"h-full rounded-full transition-[width] duration-300",
								band.bar,
							)}
							style={{ width: `${fill}%` }}
						/>
					</div>
					{/* Where the default junk threshold falls, so the score has context. */}
					<div className="relative mt-0.5 h-3 text-[10px] text-neutral-400">
						<span
							className="absolute -translate-x-1/2"
							style={{ left: `${marker}%` }}
							title={t("spam.threshold")}
						>
							▲
						</span>
					</div>
				</div>
			)}

			{tags.length > 0 && (
				<div className="mt-3.5 border-t border-neutral-100 pt-3">
					<div className="mb-2 text-xs font-medium uppercase tracking-wide text-neutral-400">
						{t("spam.signals")}
					</div>
					<div className="flex flex-wrap gap-1.5">
						{tags.map((tag) => (
							<SignalChip key={`${tag.name}-${tag.weight}`} signal={tag} />
						))}
					</div>
				</div>
			)}
		</section>
	);
}

/** One detector tag as a chip, coloured by whether it pushed toward junk. */
function SignalChip({ signal }: { signal: SpamSignal }) {
	const Icon = iconFor(signal.name);
	const chip = signal.positive
		? "border-red-200 bg-red-50 text-red-700"
		: "border-emerald-200 bg-emerald-50 text-emerald-700";
	const value = `${signal.weight >= 0 ? "+" : "\u2212"}${Math.abs(signal.weight).toFixed(2)}`;
	return (
		<span
			title={`${signal.name} (${signal.weight.toFixed(2)})`}
			className={cn(
				"inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs font-medium",
				chip,
			)}
		>
			<Icon className="size-3 shrink-0 opacity-70" aria-hidden="true" />
			<span className="uppercase tracking-tight">
				{signal.name.replace(/_/g, " ").toLowerCase()}
			</span>
			<span className="tabular-nums opacity-80">{value}</span>
		</span>
	);
}
