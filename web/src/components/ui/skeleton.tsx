import { cn } from "@/lib/utils";
import type { SkeletonProps } from "./skeleton-types";

export function Skeleton({ className, ...props }: SkeletonProps) {
	return (
		<div
			aria-hidden="true"
			className={cn("animate-pulse rounded-md bg-neutral-200/80", className)}
			{...props}
		/>
	);
}
