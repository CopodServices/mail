import { describe, expect, test } from "bun:test";
import {
	formatStorageBytes,
	storageLabel,
	storagePercent,
} from "@/components/sidebar-footer-utils";

describe("storage meter helpers", () => {
	test("formats sizes in binary units", () => {
		expect(formatStorageBytes(0)).toBe("0 B");
		expect(formatStorageBytes(-5)).toBe("0 B");
		expect(formatStorageBytes(512)).toBe("512 B");
		expect(formatStorageBytes(1024)).toBe("1.0 KB");
		expect(formatStorageBytes(1536)).toBe("1.5 KB");
		expect(formatStorageBytes(5 * 1024 * 1024)).toBe("5.0 MB");
		expect(formatStorageBytes(100 * 1024 * 1024 * 1024)).toBe("100 GB");
		expect(formatStorageBytes(Number.NaN)).toBe("0 B");
	});

	test("percent is null when unlimited and clamped otherwise", () => {
		expect(storagePercent(500, 0)).toBeNull();
		expect(storagePercent(500, 1000)).toBe(50);
		expect(storagePercent(2000, 1000)).toBe(100);
		expect(storagePercent(0, 1000)).toBe(0);
	});

	test("label reads as used/quota, or used with the unlimited word", () => {
		expect(storageLabel(1536, 4096, "Unlimited")).toBe("1.5 KB / 4.0 KB");
		expect(storageLabel(1536, 0, "Unlimited")).toBe("1.5 KB · Unlimited");
	});
});
