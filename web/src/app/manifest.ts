import type { MetadataRoute } from "next";

// Static export requires the route to be prerendered.
export const dynamic = "force-static";

export default function manifest(): MetadataRoute.Manifest {
	return {
		name: "cPOD Mail",
		short_name: "cPOD Mail",
		description: "cPOD Mail web client",
		start_url: "/inbox",
		display: "standalone",
		background_color: "#ffffff",
		theme_color: "#ffffff",
		icons: [
			{ src: "/icon-192.png", sizes: "192x192", type: "image/png" },
			{ src: "/icon-512.png", sizes: "512x512", type: "image/png" },
		],
	};
}
