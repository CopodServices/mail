"use client";

import { Check, Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { useLanguage } from "@/components/language-provider";
import { fetchClientConnection } from "@/lib/api/endpoints";
import type { ClientConnection } from "@/lib/api/types";
import type { Translator } from "@/lib/i18n/utils";

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

/** The TLS mode as a client reads it back. */
function securityLabel(t: Translator, value: string): string {
	if (value === "starttls") return t("settings.clients.starttls");
	if (value === "tls") return "TLS";
	return t("settings.clients.none");
}

/**
 * One protocol's settings, under the protocol's own name. The name is not
 * translated: IMAP, POP3 and SMTP are what the client asks for too. A protocol
 * this server does not answer says so instead of showing a dead endpoint.
 */
function ConnectionBlock({
	label,
	supported,
	host,
	port,
	security,
	username,
}: {
	label: string;
	supported: boolean;
	host: string;
	port: string;
	security: string;
	username?: string | null;
}) {
	const { t } = useLanguage();
	return (
		<div className="space-y-1">
			<h3 className="text-sm font-semibold text-neutral-900">{label}</h3>
			{supported ? (
				<dl className="divide-y divide-neutral-100">
					<ConnectionField label={t("settings.clients.server")} value={host} />
					<ConnectionField label={t("settings.clients.port")} value={port} />
					<ConnectionField
						label={t("settings.clients.security")}
						value={security}
					/>
					{username ? (
						<ConnectionField
							label={t("settings.clients.username")}
							value={username}
						/>
					) : null}
				</dl>
			) : (
				<p className="text-sm text-neutral-500">
					{t("settings.clients.notEnabled")}
				</p>
			)}
		</div>
	);
}

/**
 * The settings another mail app uses for this mailbox: the SMTP submission it
 * sends with, and the IMAP and POP3 readers when this server offers them. The
 * values are read from the server, so the hosts and ports match the deployment.
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

	return (
		<div className="space-y-6">
			<p className="text-sm text-neutral-500">
				{t("settings.clients.description")}
			</p>
			<ConnectionBlock
				label="IMAP"
				supported={connection.imapSupported}
				host={connection.imapHost}
				port={String(connection.imapPort)}
				security={securityLabel(t, connection.imapSecurity)}
				username={connection.username}
			/>
			<ConnectionBlock
				label="POP3"
				supported={connection.pop3Supported}
				host={connection.pop3Host}
				port={String(connection.pop3Port)}
				security={securityLabel(t, connection.pop3Security)}
				username={connection.username}
			/>
			<ConnectionBlock
				label="SMTP"
				supported
				host={connection.smtpHost}
				port={String(connection.smtpPort)}
				security={securityLabel(t, connection.smtpSecurity)}
				username={connection.username}
			/>
		</div>
	);
}
