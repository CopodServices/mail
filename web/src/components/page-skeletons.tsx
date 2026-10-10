import { Skeleton } from "@/components/ui/skeleton";

export function MessageDetailSkeleton() {
	return (
		<div className="h-full space-y-6 p-6">
			<div className="flex justify-between">
				<Skeleton className="h-9 w-9 rounded-full" />
				<Skeleton className="h-9 w-48" />
			</div>
			<Skeleton className="h-8 w-3/5" />
			<div className="flex items-start gap-3 border-b border-neutral-100 pb-5">
				<Skeleton className="h-10 w-10 rounded-full" />
				<div className="flex-1 space-y-2">
					<Skeleton className="h-4 w-48" />
					<Skeleton className="h-3 w-32" />
				</div>
				<Skeleton className="h-3 w-28" />
			</div>
			<div className="space-y-3">
				<Skeleton className="h-4 w-full" />
				<Skeleton className="h-4 w-11/12" />
				<Skeleton className="h-4 w-4/5" />
				<Skeleton className="h-4 w-5/6" />
			</div>
		</div>
	);
}
