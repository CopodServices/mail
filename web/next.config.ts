import type { NextConfig } from "next";

// The web client is a static, client-rendered bundle: a production build emits
// plain HTML/CSS/JS into ./out, and every page loads its data in the browser
// from the mail API. There is no Node server in production; the container
// serves the static files with nginx and reverse-proxies /api/* to the API.
const nextConfig: NextConfig = {
	output: "export",
	// Static export cannot run the image optimizer.
	images: { unoptimized: true },
	// Emit folder/index.html per route so a plain static host resolves deep links.
	trailingSlash: true,
	reactStrictMode: true,
	typescript: {
		// The app was ported from a larger codebase; keep type errors from blocking
		// the bundle until the port is fully typed.
		ignoreBuildErrors: true,
	},
};

export default nextConfig;
