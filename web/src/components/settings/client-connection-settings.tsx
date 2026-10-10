"use client";

import { Check, Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { fetchClientConnection } from "@/lib/api/endpoints";
import type { ClientConnection } from "@/lib/api/types";

/** One copyable row of the connection table. */
function ConnectionField({ label, value }: { label: string; value: string }) {
	const { t } = useLanguage();
	const [copied, setCopied] = useState(false);

	async function copy() {
		try {
			await navigator.clipboard.writeText(value);
			setCopied(true);
			window.setTimeout(() => setCopied(false), 1500);
		} catch {
			setCopied(false);
		}
	}

	return (
		<div className="flex items-center justify-between gap-3 py-3">
			<dt className="text-sm text-neutral-500">{label}</dt>
			<dd className="flex min-w-0 items-center gap-2">
				<span className="truncate font-mono text-sm text-neutral-900">
					{value}
				</span>
				<button
					type="button"
					onClick={() => void copy()}
					aria-label={t("source.copy")}
					title={copied ? t("source.copied") : t("source.copy")}
					className="shrink-0 rounded-md p-1.5 text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-neutral-900"
				>
					{copied ? (
						<Check className="h-4 w-4" />
					) : (
						<Copy className="h-4 w-4" />
					)}
				</button>
			</dd>
		</div>
	);
}

/**
 * The submission settings another mail app uses to send as this mailbox. The
 * values are read from the server, so the host and port match the deployment.
 */
export function ClientConnectionSettings() {
	const { t } = useLanguage();
	const [connection, setConnection] = useState<ClientConnection | null>(null);
	const [failed, setFailed] = useState(false);

	useEffect(() => {
		let cancelled = false;
		fetchClientConnection()
			.then((next) => {
				if (!cancelled) setConnection(next);
			})
			.catch(() => {
				if (!cancelled) setFailed(true);
			});
		return () => {
			cancelled = true;
		};
	}, []);

	if (failed)
		return (
			<p className="text-sm text-red-600">{t("settings.clients.loadFailed")}</p>
		);
	if (!connection) return <p className="text-sm text-neutral-500">…</p>;

	const security =
		connection.smtpSecurity === "starttls"
			? t("settings.clients.starttls")
			: t("settings.clients.none");

	return (
		<div className="space-y-4">
			<p className="text-sm text-neutral-500">
				{t("settings.clients.description")}
			</p>
			<dl className="divide-y divide-neutral-100">
				<ConnectionField
					label={t("settings.clients.server")}
					value={connection.smtpHost}
				/>
				<ConnectionField
					label={t("settings.clients.port")}
					value={String(connection.smtpPort)}
				/>
				<ConnectionField
					label={t("settings.clients.security")}
					value={security}
				/>
				{connection.username && (
					<ConnectionField
						label={t("settings.clients.username")}
						value={connection.username}
					/>
				)}
			</dl>
		</div>
	);
}
