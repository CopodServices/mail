export type RecipientInputProps = {
	id: string;
	label: string;
	value: string[];
	onChange: (next: string[]) => void;
	placeholder?: string;
	disabled?: boolean;
	required?: boolean;
	/** Domain appended to a bare mailbox name, e.g. `maya` -> `maya@example.com`. */
	defaultDomain?: string | null;
	/** Focus the field when it first appears, e.g. after the Cc/Bcc toggle reveals it. */
	autoFocus?: boolean;
	/** Rendered at the right edge of the row, e.g. the Cc/Bcc toggles. */
	trailing?: React.ReactNode;
};
