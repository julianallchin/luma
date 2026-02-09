import type { Metadata } from "next";
import { Inter } from "next/font/google";
import "./marketing.css";

const inter = Inter({
	variable: "--font-inter",
	subsets: ["latin"],
});

export const metadata: Metadata = {
	title: "Luma - Semantic Lighting Control",
	description:
		"Design your show once, perform it anywhere. Semantic lighting control for any venue.",
};

export default function MarketingLayout({
	children,
}: Readonly<{
	children: React.ReactNode;
}>) {
	return (
		<html lang="en">
			<body className={`${inter.variable} antialiased bg-bg text-fg font-sans`}>
				{children}
			</body>
		</html>
	);
}
