"use client";

import clsx from "clsx";
import { Clock } from "lucide-react";
import { useLanguage } from "@/components/language-provider";
import { isMessageSnoozed } from "@/components/messages/message-list-row-actions-utils";
import { formatScheduledSendTime } from "@/components/messages/scheduled-send-utils";
import type { MessageStateChipsProps } from "./message-state-chips-types";
import { getSendStateKey } from "./utils";

/**
 * What a row says about the message beyond its sender and subject: a delivery
 * that has not gone out yet, one that failed, or one that is snoozed until a
 * time. A delivered message carries no chip.
 */
export function MessageStateChips({
	message,
	className,
}: MessageStateChipsProps) {
	const { t } = useLanguage();
	const sendKey = getSendStateKey(message.sendState);
	const snoozed = isMessageSnoozed(message.snoozedUntil);
	if (!sendKey && !snoozed) return null;

	return (
		<span className={clsx("flex shrink-0 items-center gap-1", className)}>
			{sendKey && (
				<span
					title={
						message.sendState === "failed"
							? (message.sendError ?? undefined)
							: undefined
					}
					className={clsx(
						"rounded-full px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide",
						message.sendState === "failed"
							? "bg-red-100 text-red-700"
							: "bg-neutral-100 text-neutral-600",
					)}
				>
					{t(sendKey)}
				</span>
			)}
			{snoozed && message.snoozedUntil && (
				<span className="flex items-center gap-1 rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-medium text-neutral-600">
					<Clock className="h-3 w-3" />
					{t("list.snoozedUntil", {
						time: formatScheduledSendTime(message.snoozedUntil, t),
					})}
				</span>
			)}
		</span>
	);
}

export default MessageStateChips;
