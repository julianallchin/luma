import { DocsLayout } from "fumadocs-ui/layouts/docs";
import type { ReactNode } from "react";
import { source } from "@/lib/source";

export default function Layout({ children }: { children: ReactNode }) {
	return (
		<DocsLayout
			tree={source.pageTree}
			nav={{
				title: <span className="font-serif text-2xl">Luma</span>,
				url: "/",
			}}
			themeSwitch={{ enabled: false }}
			sidebar={{
				defaultOpenLevel: 0,
			}}
		>
			{children}
		</DocsLayout>
	);
}
