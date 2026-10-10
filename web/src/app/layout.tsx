import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import { LanguageProvider } from "@/components/language-provider";
import { Providers } from "@/components/providers";
import { sidebarBootstrapScript } from "@/components/sidebar-state-utils";
import { themeBootstrapScript } from "@/components/theme-utils";
import { DEFAULT_LOCALE } from "@/lib/i18n/utils";
import "./globals.css";

const geistSans = Geist({
	variable: "--font-geist-sans",
	subsets: ["latin"],
});

const geistMono = Geist_Mono({
	variable: "--font-geist-mono",
	subsets: ["latin"],
});

export const metadata: Metadata = {
	title: "cPOD Mail",
	description: "cPOD Mail web client",
	icons: { icon: "/icon-96.png", apple: "/icon-192.png" },
	appleWebApp: {
		capable: true,
		title: "cPOD Mail",
		statusBarStyle: "default",
	},
	robots: {
		index: false,
		follow: false,
		noarchive: true,
		nosnippet: true,
		noimageindex: true,
		googleBot: {
			index: false,
			follow: false,
			noarchive: true,
			nosnippet: true,
			noimageindex: true,
		},
	},
};

// Fully client-rendered: the shell ships as static HTML and every page loads its
// data in the browser from the mail API. The saved language and theme are
// applied by the bootstrap scripts before React hydrates.
export default function RootLayout({
	children,
}: Readonly<{ children: React.ReactNode }>) {
	return (
		<html lang={DEFAULT_LOCALE} dir="ltr" suppressHydrationWarning>
			<head>
				<script dangerouslySetInnerHTML={{ __html: sidebarBootstrapScript }} />
				<script dangerouslySetInnerHTML={{ __html: themeBootstrapScript }} />
				<link rel="icon" href="/icon-96.png"></link>
			</head>
			<body
				className={
					geistSans.variable + " " + geistMono.variable + " antialiased"
				}
			>
				<LanguageProvider initialLocale={DEFAULT_LOCALE}>
					<Providers>{children}</Providers>
				</LanguageProvider>
			</body>
		</html>
	);
}
