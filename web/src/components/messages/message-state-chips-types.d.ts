import type { Message } from "@/hooks/types";

export type MessageStateChipsProps = {
	message: Pick<Message, "snoozedUntil" | "sendState" | "sendError">;
	className?: string;
};
