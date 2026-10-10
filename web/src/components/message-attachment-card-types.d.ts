import type { TranslationKey } from "@/lib/i18n/types";
import type { MessageAttachment } from "@/components/messages/message-detail/types";
import type { LucideIcon } from "lucide-react";

export interface MessageAttachmentCardProps {
	attachment: MessageAttachment;
	messageId: string;
	onPreview: (attachment: MessageAttachment) => void;
}

export type AttachmentVisual = {
	icon: LucideIcon;
	iconClassName: string;
	labelKey: TranslationKey;
	thumbnail: "image" | "video" | null;
};
