import { RootProvider } from "fumadocs-ui/provider/next";
import type { Metadata } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";
import "../globals.css";

const inter = Inter({
	variable: "--font-inter",
	subsets: ["latin"],
});

const jetbrainsMono = JetBrains_Mono({
	variable: "--font-jetbrains-mono",
	subsets: ["latin"],
});

export const metadata: Metadata = {
	title: {
		template: "%s | Luma Docs",
		default: "Luma Documentation",
	},
	description: "Documentation for Luma - Semantic Lighting Control",
};

export default function DocsRootLayout({
	children,
}: Readonly<{
	children: React.ReactNode;
}>) {
	return (
		<html lang="en" suppressHydrationWarning className="dark">
			<body
				className={`${inter.variable} ${jetbrainsMono.variable} flex min-h-screen flex-col antialiased`}
			>
				<RootProvider>{children}</RootProvider>
			</body>
		</html>
	);
}
