import Image from "next/image";
import Link from "next/link";
import { Download } from "./download";

const SOURCE = "https://github.com/lumashow/luma";

export default function LandingPage() {
	return (
		<main className="hero">
			<div className="sky" aria-hidden="true" />

			<header className="bar">
				<span className="wordmark">Luma</span>
				<nav aria-label="Site">
					<Link href="/docs">Docs</Link>
					<a href={SOURCE}>GitHub</a>
				</nav>
			</header>

			<div className="pitch">
				<h1>Lights that know the song.</h1>
				<p>
					Luma listens to your music and builds the light show with you. No
					lighting experience needed. Say what you want and watch it happen.
				</p>
				<Download />
			</div>

			<div className="window">
				<Image
					src="/screenshots/arrange.webp"
					alt="Luma in Arrange: the agent's notes on the show it wrote at the left, the stage lit in red under two white beams, and the score's clips in lanes under the song's waveform."
					width={2000}
					height={1248}
					priority
					sizes="(orientation: portrait) 130vw, (min-width: 2000px) 60vw, (min-width: 1296px) 1200px, 100vw"
				/>
			</div>
		</main>
	);
}
