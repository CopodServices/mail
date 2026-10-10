"use client";

import { useRouter } from "next/navigation";
import { useEffect } from "react";

/** Static export has no server redirect, so the entry point routes client-side. */
export default function HomePage() {
	const router = useRouter();
	useEffect(() => {
		router.replace("/inbox");
	}, [router]);
	return null;
}
