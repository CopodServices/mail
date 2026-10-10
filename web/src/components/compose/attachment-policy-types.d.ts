export interface ComposeAttachmentPolicy {
	maxMb: number;
	/** Over this the composer warns that the message may exceed the size budget. */
	warnThresholdBytes: number;
}
