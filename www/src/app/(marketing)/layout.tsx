import type { Metadata } from "next";
import { Inter, Newsreader } from "next/font/google";
import "./marketing.css";

// Newsreader is the page's voice; Inter is the app's, for anything that is a
// control or a name out of the app.
const newsreader = Newsreader({
	variable: "--font-newsreader",
	subsets: ["latin"],
	style: ["normal", "italic"],
});

const inter = Inter({
	variable: "--font-inter",
	subsets: ["latin"],
});

const description =
	"Luma listens to your music and builds the light show with you. A 3D visualizer, a fixture library and lasers, free, with no lighting experience needed.";

export const metadata: Metadata = {
	metadataBase: new URL("https://luma.show"),
	title: "Luma: lights that know the song",
	description,
	openGraph: {
		title: "Luma: lights that know the song",
		description,
		images: ["/screenshots/hero.webp"],
	},
};

export default function MarketingLayout({
	children,
}: Readonly<{
	children: React.ReactNode;
}>) {
	return (
		<html lang="en" className={`${newsreader.variable} ${inter.variable}`}>
			<body>{children}</body>
		</html>
	);
}
