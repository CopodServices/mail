import type { Branding } from "@/lib/branding/types";

export const DEFAULT_BRANDING: Branding = {
	appName: "cPOD Mail",
	hasCustomIcon: false,
	canCustomizeBranding: false,
};

export async function fetchBranding(): Promise<Branding> {
	// Branding is fixed in the web client; there is no branding endpoint.
	return DEFAULT_BRANDING;
}
