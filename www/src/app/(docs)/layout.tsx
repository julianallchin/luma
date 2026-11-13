import { RootProvider } from "fumadocs-ui/provider/next";
import type { Metadata } from "next";
import { Inter, JetBrains_Mono, Newsreader } from "next/font/google";
import "../globals.css";

const inter = Inter({
	variable: "--font-inter",
	subsets: ["latin"],
});

const newsreader = Newsreader({
	variable: "--font-newsreader",
	subsets: ["latin"],
});

const jetbrainsMono = JetBrains_Mono({
	variable: "--font-jetbrains-mono",
	subsets: ["latin"],
});

export const metadata: Metadata = {
	metadataBase: new URL("https://luma.show"),
	title: {
		template: "%s · Luma docs",
		default: "Luma docs",
	},
	description: "How to build a rig, write a score and play it live in Luma.",
};

export default function DocsRootLayout({
	children,
}: Readonly<{
	children: React.ReactNode;
}>) {
	return (
		<html
			lang="en"
			suppressHydrationWarning
			className={`dark ${inter.variable} ${newsreader.variable} ${jetbrainsMono.variable}`}
		>
			<body className="flex min-h-screen flex-col antialiased">
				{/* Five pages need no search, and a static site has no search route. */}
				<RootProvider theme={{ enabled: false }} search={{ enabled: false }}>
					{children}
				</RootProvider>
			</body>
		</html>
	);
}
