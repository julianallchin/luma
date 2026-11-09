import { ArrowRight, Download } from "lucide-react";
import Image from "next/image";
import Link from "next/link";

const DOWNLOAD = "https://github.com/lumashow/luma/releases/latest";

const features = [
	{
		image: "/screenshots/arrange.webp",
		alt: "Arrange mode: the agent panel on the left, the lit stage with the song and score menu at its top, and the score's clips in lanes under the song's waveform.",
		title: "Arrange a score against the song",
		body: "Luma analyzes each song's beats, stems and chords. Place clips on its timeline, each one a small graph that turns the music into colour, movement, strobe or laser figures. Ask the agent on the left to build or change a score with you.",
		link: { href: "/docs/user-guide/patterns", label: "Clips and presets" },
	},
	{
		image: "/screenshots/live.webp",
		alt: "Live mode: the deck panel playing a song on the virtual deck beside the stage, and a page of pads for washes, chases, laser looks and strobes under them.",
		title: "Play it live",
		body: "Live follows the DJ over Pro DJ Link, StageLinQ or Rekordbox and plays each song's score on the beat. Pages of pads, faders and colour cells sit beside it for busking, and a MIDI or OSC controller can play them.",
		link: { href: "/docs/user-guide/performing", label: "Live" },
	},
	{
		image: "/screenshots/rig.webp",
		alt: "Rig mode: a venue's laser projectors and LED bars on truss towers, the environment and haze controls beside the 3D view, and the patch and groups under it.",
		title: "Build the room once",
		body: "Hang fixtures and laser projectors in 3D, patch them, and name groups such as back_beams or lasers. Scores aim at groups, never at fixtures, so a show carries from one venue to the next.",
		link: { href: "/docs/user-guide/venues", label: "Venues" },
	},
	{
		image: "/screenshots/songs.webp",
		alt: "The song list: the venue's songs with their cover art, and the selected song's scores in each venue.",
		title: "Every song, every score",
		body: "The song list is a column view: places and folders, their songs, and each song's scores for this venue. Press ⌘P to bring it up from anywhere.",
		link: { href: "/docs/user-guide/the-window", label: "The window" },
	},
];

export default function LandingPage() {
	return (
		<div className="min-h-screen">
			<nav className="w-full px-6 md:px-12 py-5 flex items-center justify-between">
				<span className="text-lg font-bold tracking-tight">LUMA</span>
				<div className="flex items-center gap-6 text-sm">
					<Link href="/docs" className="text-fg/60 hover:text-fg transition-colors">
						Docs
					</Link>
					<a href={DOWNLOAD} className="text-fg/60 hover:text-fg transition-colors">
						Download
					</a>
				</div>
			</nav>

			<header className="px-6 md:px-12 pt-12 md:pt-20 text-center">
				<h1 className="text-4xl md:text-6xl font-bold tracking-tight leading-[1.1]">
					Sheet music for light.
				</h1>
				<p className="mt-5 text-sm md:text-base text-fg/60 leading-relaxed max-w-lg mx-auto">
					Design your show once and it adapts to any venue. Your creative intent
					travels with you; the room provides the instruments.
				</p>
				<div className="mt-7 flex items-center justify-center gap-5">
					<a
						href={DOWNLOAD}
						className="inline-flex items-center gap-2 rounded-full bg-primary text-bg px-5 py-2.5 text-sm font-medium transition-colors hover:bg-accent"
					>
						<Download size={14} />
						Download
					</a>
					<Link
						href="/docs/user-guide/why-luma"
						className="group inline-flex items-center gap-2 text-sm text-fg/50 hover:text-fg transition-colors"
					>
						Why Luma?
						<ArrowRight size={14} className="group-hover:translate-x-0.5 transition-transform" />
					</Link>
				</div>
			</header>

			<div className="px-4 md:px-12 mt-12 md:mt-16">
				<Image
					src="/screenshots/hero.webp"
					alt="Luma in Arrange mode: a song's dense score under the lit stage, with the agent chat on the left."
					width={2400}
					height={1560}
					priority
					sizes="(min-width: 1280px) 1200px, 100vw"
					className="mx-auto w-full max-w-6xl h-auto rounded-2xl"
				/>
			</div>

			<main className="px-6 md:px-12 py-20 md:py-28 space-y-24 md:space-y-32 max-w-6xl mx-auto">
				{features.map((feature, index) => (
					<section
						key={feature.title}
						className="grid md:grid-cols-5 gap-8 md:gap-12 items-center"
					>
						<div className={`md:col-span-2 space-y-4 ${index % 2 ? "md:order-2" : ""}`}>
							<h2 className="text-2xl md:text-3xl font-bold tracking-tight">{feature.title}</h2>
							<p className="text-sm md:text-base text-fg/60 leading-relaxed">{feature.body}</p>
							<Link
								href={feature.link.href}
								className="group inline-flex items-center gap-2 text-sm text-primary hover:text-fg transition-colors"
							>
								{feature.link.label}
								<ArrowRight size={14} className="group-hover:translate-x-0.5 transition-transform" />
							</Link>
						</div>
						<Image
							src={feature.image}
							alt={feature.alt}
							width={2000}
							height={1320}
							sizes="(min-width: 768px) 60vw, 100vw"
							className="md:col-span-3 w-full h-auto rounded-xl"
						/>
					</section>
				))}
			</main>

			<footer className="px-6 md:px-12 pb-20 text-center space-y-5">
				<h2 className="text-2xl md:text-3xl font-bold tracking-tight">Light the next show.</h2>
				<div className="flex items-center justify-center gap-5">
					<a
						href={DOWNLOAD}
						className="inline-flex items-center gap-2 rounded-full bg-primary text-bg px-5 py-2.5 text-sm font-medium transition-colors hover:bg-accent"
					>
						<Download size={14} />
						Download
					</a>
					<Link
						href="/docs"
						className="group inline-flex items-center gap-2 text-sm text-fg/50 hover:text-fg transition-colors"
					>
						Read the docs
						<ArrowRight size={14} className="group-hover:translate-x-0.5 transition-transform" />
					</Link>
				</div>
			</footer>
		</div>
	);
}
