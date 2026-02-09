import { ArrowRight } from "lucide-react";
import Image from "next/image";
import Link from "next/link";

export default function LandingPage() {
	return (
		<div className="h-screen overflow-hidden relative flex flex-col">
			{/* Background image */}
			<Image
				src="/hero.png"
				alt=""
				fill
				priority
				className="object-cover object-center"
			/>

			{/* Gradient overlays */}
			<div className="absolute inset-0 bg-bg/40" />
			<div className="absolute inset-0 bg-gradient-to-b from-bg/80 via-transparent to-bg" />

			{/* Nav */}
			<nav className="relative z-10 w-full px-6 md:px-12 py-5 flex items-center justify-between">
				<span className="text-lg font-bold tracking-tight">LUMA</span>
				<Link
					href="/docs"
					className="text-sm text-fg/60 hover:text-fg transition-colors"
				>
					Docs
				</Link>
			</nav>

			{/* Hero content */}
			<div className="relative z-10 flex-1 flex items-end px-6 md:px-12 pb-16 md:pb-24">
				<div className="max-w-xl space-y-5">
					<h1 className="text-4xl md:text-6xl font-bold tracking-tight leading-[1.1]">
						Sheet music
						<br />
						for light.
					</h1>

					<p className="text-sm md:text-base text-fg/60 leading-relaxed max-w-sm">
						Design your show once — it adapts to any venue. Your creative intent
						travels with you. The room provides the instruments.
					</p>

					<div className="flex items-center gap-4 pt-1">
						<Link
							href="/docs"
							className="group inline-flex items-center gap-2 bg-primary text-bg px-5 py-2.5 text-sm font-medium transition-colors hover:bg-accent"
						>
							Read the docs
							<ArrowRight
								size={14}
								className="group-hover:translate-x-0.5 transition-transform"
							/>
						</Link>
						<Link
							href="/docs/user-guide/why-luma"
							className="text-sm text-fg/40 hover:text-fg transition-colors"
						>
							Why Luma?
						</Link>
					</div>
				</div>
			</div>
		</div>
	);
}
