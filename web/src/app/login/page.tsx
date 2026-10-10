"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { fetchSession, signIn } from "@/lib/api/session";
import { DEFAULT_AFTER_LOGIN, safeNext } from "@/lib/api/session-utils";
import type { TranslationKey } from "@/lib/i18n/types";

/**
 * The webmail sign-in page. The mailbox password is checked by the mail API,
 * which then sets the session cookie; the browser holds no secret of its own.
 */
export default function LoginPage() {
	const router = useRouter();
	const { t } = useLanguage();
	const [address, setAddress] = useState("");
	const [password, setPassword] = useState("");
	const [error, setError] = useState<TranslationKey | null>(null);
	const [busy, setBusy] = useState(false);

	// A visitor who already has a session goes straight to their mail.
	useEffect(() => {
		void fetchSession().then((session) => {
			if (session) router.replace(DEFAULT_AFTER_LOGIN);
		});
	}, [router]);

	async function submit(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		if (busy) return;
		setBusy(true);
		setError(null);
		try {
			await signIn(address.trim(), password);
			const next = new URLSearchParams(window.location.search).get("next");
			router.replace(safeNext(next));
		} catch (err) {
			setError(
				(err instanceof Error
					? err.message
					: "session.error.generic") as TranslationKey,
			);
		} finally {
			setBusy(false);
		}
	}

	return (
		<main className="flex min-h-dvh items-center justify-center bg-[#f6f8fc] px-4">
			<form
				onSubmit={submit}
				className="w-full max-w-sm rounded-2xl bg-white p-8 shadow-sm"
			>
				<h1 className="text-xl font-semibold text-neutral-900">
					{t("session.signInTitle")}
				</h1>
				<p className="mt-1 text-sm text-neutral-500">
					{t("session.signInDescription")}
				</p>
				<div className="mt-6 space-y-4">
					<div className="space-y-1.5">
						<Label htmlFor="address">{t("session.address")}</Label>
						<Input
							id="address"
							name="address"
							type="email"
							autoComplete="username"
							required
							value={address}
							onChange={(event) => setAddress(event.target.value)}
						/>
					</div>
					<div className="space-y-1.5">
						<Label htmlFor="password">{t("session.password")}</Label>
						<Input
							id="password"
							name="password"
							type="password"
							autoComplete="current-password"
							required
							value={password}
							onChange={(event) => setPassword(event.target.value)}
						/>
					</div>
				</div>
				{error && (
					<p className="mt-4 text-sm text-red-600" role="alert">
						{t(error)}
					</p>
				)}
				<Button type="submit" className="mt-6 w-full" disabled={busy}>
					{busy ? t("session.signingIn") : t("session.signIn")}
				</Button>
			</form>
		</main>
	);
}
