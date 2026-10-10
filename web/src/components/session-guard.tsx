"use client";

import { usePathname, useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { fetchSession } from "@/lib/api/session";
import { loginHref } from "@/lib/api/session-utils";

/**
 * Client-side session gate for the dashboard. A static export has no server
 * middleware, so the check runs in the browser: the chrome is withheld until
 * the session resolves, and an unauthenticated visitor goes to /login with the
 * page they wanted as ?next=.
 */
export function SessionGuard({ children }: { children: React.ReactNode }) {
	const router = useRouter();
	const pathname = usePathname();
	const { t } = useLanguage();
	const [signedIn, setSignedIn] = useState(false);

	useEffect(() => {
		let cancelled = false;
		void fetchSession().then((session) => {
			if (cancelled) return;
			if (session) {
				setSignedIn(true);
				return;
			}
			router.replace(loginHref(pathname));
		});
		return () => {
			cancelled = true;
		};
	}, [router, pathname]);

	if (!signedIn) {
		return (
			<div className="flex h-dvh items-center justify-center bg-[#f6f8fc] text-sm text-neutral-500">
				{t("session.loading")}
			</div>
		);
	}

	return <>{children}</>;
}
