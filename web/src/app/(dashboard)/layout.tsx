"use client";

import { Suspense } from "react";
import { ComposeProvider } from "@/components/compose/compose-context";
import { FloatingComposer } from "@/components/compose/floating-composer";
import { DashboardNav } from "@/components/dashboard-nav";
import { MailSearchProvider } from "@/components/mail-search/mail-search-context";
import { MailSearchInput } from "@/components/mail-search/mail-search-input";
import { MailboxProvider } from "@/components/mailbox-provider";
import { MailboxSelector } from "@/components/mailbox-selector";
import { ShortcutsProvider } from "@/components/shortcuts";
import { MobileMenuButton, SidebarAside } from "@/components/sidebar-aside";
import { SidebarResizeBoundary } from "@/components/sidebar-resize-boundary";
import { SidebarProvider } from "@/components/sidebar-state";

export default function DashboardLayout({
	children,
}: {
	children: React.ReactNode;
}) {
	return (
		<SidebarProvider mobileOverlay>
			<MailboxProvider>
				<ComposeProvider>
					<MailSearchProvider>
						<ShortcutsProvider>
							<div
								className="grid h-dvh grid-cols-[minmax(0,1fr)] overflow-hidden bg-[#f6f8fc] transition-[grid-template-columns] md:grid-cols-[var(--sidebar-width)_minmax(0,1fr)]"
								style={{
									transitionDuration: "var(--sidebar-transition-duration)",
								}}
							>
								<SidebarAside>
									<div className="h-full">
										<Suspense fallback={null}>
											<DashboardNav />
										</Suspense>
									</div>
									<SidebarResizeBoundary />
								</SidebarAside>
								<div className="flex min-h-0 min-w-0 flex-col">
									<header className="flex h-16 w-full shrink-0 items-center gap-3 pr-4 text-sm">
										<MobileMenuButton className="ml-2" />
										<MailSearchInput />
										<MailboxSelector />
									</header>
									<div className="flex min-h-0 min-w-0 flex-1 overflow-hidden">
										<main className="min-h-0 min-w-0 flex-1 overflow-y-auto rounded-t-3xl bg-white overscroll-contain scrollbar-gutter-stable">
											{children}
										</main>
									</div>
								</div>
								<FloatingComposer />
							</div>
						</ShortcutsProvider>
					</MailSearchProvider>
				</ComposeProvider>
			</MailboxProvider>
		</SidebarProvider>
	);
}
