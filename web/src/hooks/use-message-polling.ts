import { useCallback, useEffect, useState } from "react";
import { hasSession, SESSION_CHANGED_EVENT } from "@/lib/api/session-utils";
import type {
	MessageRealtimeState,
	NewMessageEvent,
	RealtimeChannelMessage,
} from "./message-realtime-types";
import {
	getRealtimeWebSocketUrl,
	getReconnectDelay,
	parseNewMessageEvent,
	REALTIME_FALLBACK_INTERVAL_MS,
	REALTIME_HEARTBEAT_INTERVAL_MS,
	showBrowserNewMessageNotification,
} from "./message-realtime-utils";

export function useMessagePolling(): MessageRealtimeState {
	const [notification, setNotification] = useState<NewMessageEvent | null>(
		null,
	);
	const dismissNotification = useCallback(() => setNotification(null), []);

	// The socket authenticates with the session cookie, so it must not open a
	// handshake while nobody is signed in — the API answers 401 and the browser
	// logs a failed connection on the login page. The readable CSRF cookie is
	// set and cleared with the session, so its presence tracks a session, and
	// sign-in/sign-out announce the change without a reload.
	const [signedIn, setSignedIn] = useState(false);
	useEffect(() => {
		if (typeof window === "undefined") return;
		const sync = () => setSignedIn(hasSession());
		sync();
		window.addEventListener(SESSION_CHANGED_EVENT, sync);
		return () => window.removeEventListener(SESSION_CHANGED_EVENT, sync);
	}, []);

	useEffect(() => {
		if (!signedIn) return;
		let socket: WebSocket | null = null;
		let channel: BroadcastChannel | null = null;
		let lockAbort: AbortController | null = null;
		let releaseLeader: (() => void) | null = null;
		let reconnectTimer: number | null = null;
		let heartbeatTimer: number | null = null;
		let fallbackTimer: number | null = null;
		let reconnectAttempt = 0;
		let revision: string | null = null;
		let connected = false;
		let leader = false;
		let stopped = false;

		function dispatchMessagesChanged() {
			window.dispatchEvent(
				new CustomEvent("copod:messages-changed", {
					detail: { realtime: true },
				}),
			);
		}

		function clearConnectionTimers() {
			if (reconnectTimer) window.clearTimeout(reconnectTimer);
			if (heartbeatTimer) window.clearInterval(heartbeatTimer);
			reconnectTimer = null;
			heartbeatTimer = null;
		}

		function startFallbackRefresh() {
			if (fallbackTimer) return;
			fallbackTimer = window.setInterval(() => {
				if (!connected && document.visibilityState === "visible")
					dispatchMessagesChanged();
			}, REALTIME_FALLBACK_INTERVAL_MS);
		}

		function setConnected(value: boolean) {
			connected = value;
			if (value) {
				if (fallbackTimer) window.clearInterval(fallbackTimer);
				fallbackTimer = null;
			} else {
				startFallbackRefresh();
			}
			if (leader)
				channel?.postMessage({
					type: "status",
					connected: value,
				} satisfies RealtimeChannelMessage);
		}

		function handleNotification(payload: string, fromSocket: boolean) {
			try {
				const parsed = JSON.parse(payload) as {
					type?: string;
					revision?: string;
					changed?: boolean;
					draftId?: string;
					mailboxId?: string;
				};
				if (parsed.type === "revision" && typeof parsed.revision === "string") {
					revision = parsed.revision;
					if (parsed.changed) {
						dispatchMessagesChanged();
						if (fromSocket)
							channel?.postMessage({
								type: "refresh",
							} satisfies RealtimeChannelMessage);
					}
					return;
				}
			} catch {
				return;
			}
			const event = parseNewMessageEvent(payload);
			if (!event) return;
			dispatchMessagesChanged();
			setNotification(event);
			if (fromSocket) {
				void showBrowserNewMessageNotification(event);
				channel?.postMessage({
					type: "notification",
					payload,
				} satisfies RealtimeChannelMessage);
			}
		}

		function sendHeartbeat() {
			if (socket?.readyState === WebSocket.OPEN)
				socket.send(JSON.stringify({ type: "ping", revision }));
		}

		function scheduleReconnect() {
			setConnected(false);
			if (stopped || !leader || false) return;
			const delay = getReconnectDelay(reconnectAttempt);
			reconnectAttempt += 1;
			reconnectTimer = window.setTimeout(connect, delay);
		}

		function connect() {
			clearConnectionTimers();
			if (stopped || !leader || false) return;

			socket = new WebSocket(getRealtimeWebSocketUrl());
			socket.onopen = () => {
				reconnectAttempt = 0;
				revision = null;
				setConnected(true);
				dispatchMessagesChanged();
				sendHeartbeat();
				heartbeatTimer = window.setInterval(
					sendHeartbeat,
					REALTIME_HEARTBEAT_INTERVAL_MS,
				);
			};
			socket.onmessage = (message) => {
				if (message.data === "pong" || typeof message.data !== "string") return;
				handleNotification(message.data, true);
			};
			socket.onerror = () => socket?.close();
			socket.onclose = () => {
				clearConnectionTimers();
				scheduleReconnect();
			};
		}

		function requestLeadership() {
			if (stopped || false) return;
			if (!channel || !navigator.locks) {
				leader = true;
				connect();
				return;
			}
			lockAbort = new AbortController();
			void navigator.locks
				.request("copod:realtime", { signal: lockAbort.signal }, async () => {
					if (stopped || false) return;
					await new Promise<void>((resolve) => {
						releaseLeader = resolve;
						leader = true;
						connect();
					});
					leader = false;
					releaseLeader = null;
				})
				.catch(() => {});
		}

		function onChannelMessage(event: MessageEvent<RealtimeChannelMessage>) {
			const message = event.data;
			if (message?.type === "status_request" && leader) {
				channel?.postMessage({
					type: "status",
					connected,
				} satisfies RealtimeChannelMessage);
			} else if (message?.type === "status" && !leader) {
				setConnected(message.connected);
				if (message.connected) dispatchMessagesChanged();
			} else if (message?.type === "notification" && !leader) {
				handleNotification(message.payload, false);
			} else if (message?.type === "refresh") {
				dispatchMessagesChanged();
			}
		}

		function onLocalMessagesChanged(event: Event) {
			if ((event as CustomEvent<{ realtime?: boolean }>).detail?.realtime)
				return;
			channel?.postMessage({
				type: "refresh",
			} satisfies RealtimeChannelMessage);
		}

		function onVisibilityChange() {
			if (document.visibilityState === "visible") dispatchMessagesChanged();
		}

		if (typeof BroadcastChannel !== "undefined") {
			channel = new BroadcastChannel("copod:realtime");
			channel.onmessage = onChannelMessage;
			channel.postMessage({
				type: "status_request",
			} satisfies RealtimeChannelMessage);
		}
		window.addEventListener("copod:messages-changed", onLocalMessagesChanged);
		document.addEventListener("visibilitychange", onVisibilityChange);
		window.addEventListener("focus", onVisibilityChange);
		startFallbackRefresh();
		requestLeadership();

		return () => {
			stopped = true;
			window.removeEventListener(
				"copod:messages-changed",
				onLocalMessagesChanged,
			);
			document.removeEventListener("visibilitychange", onVisibilityChange);
			window.removeEventListener("focus", onVisibilityChange);
			if (leader)
				channel?.postMessage({
					type: "status",
					connected: false,
				} satisfies RealtimeChannelMessage);
			lockAbort?.abort();
			releaseLeader?.();
			clearConnectionTimers();
			if (fallbackTimer) window.clearInterval(fallbackTimer);
			channel?.close();
			if (socket) {
				socket.onclose = null;
				socket.close(1000, "Client closed");
			}
		};
	}, [signedIn]);

	useEffect(() => {
		if (!notification) return;
		const timer = window.setTimeout(() => setNotification(null), 8_000);
		return () => window.clearTimeout(timer);
	}, [notification]);

	return { notification, dismissNotification };
}
