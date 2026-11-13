"use client";

import Link from "next/link";
import { useRef, useState, useSyncExternalStore } from "react";

// The release workflow publishes these names unchanged for every version, so
// `releases/latest/download/` is always the newest stable build.
const LATEST = "https://github.com/lumashow/luma/releases/latest/download";
const DMG = `${LATEST}/Luma-aarch64.dmg`;
const INSTALL = `curl -fsSL ${LATEST}/install.sh | sh`;

type Platform = "mac" | "linux" | "windows";

// A phone, a tablet or anything unrecognised reads as a Mac: the page is
// then an advertisement, and the Mac build is the one most people want.
function platform(): Platform {
	const agent = navigator.userAgent;
	if (/Android|CrOS/.test(agent)) return "mac";
	if (/Linux|X11/.test(agent)) return "linux";
	if (/Windows/.test(agent)) return "windows";
	return "mac";
}

const never = () => () => {};

/** The hero's actions: the newest build for the visitor's computer. */
export function Download() {
	// The server renders the Mac button; the browser corrects it on arrival.
	const on = useSyncExternalStore<Platform>(never, platform, () => "mac");
	const [said, setSaid] = useState<string>();
	const command = useRef<HTMLElement>(null);

	const copy = async () => {
		try {
			await navigator.clipboard.writeText(INSTALL);
			setSaid("Copied");
		} catch {
			// No clipboard here (a page not served over HTTPS, or a refusal):
			// select the command, so the visitor's own copy key takes it.
			if (command.current) {
				window.getSelection()?.selectAllChildren(command.current);
			}
			setSaid("Selected. Press Ctrl+C");
		}
		setTimeout(() => setSaid(undefined), 2500);
	};

	return (
		<>
			<div className="actions">
				{on === "linux" ? (
					<button type="button" className="download" onClick={copy}>
						{said ?? "Copy install command"}
					</button>
				) : (
					<a className="download" href={DMG}>
						Download for Mac
					</a>
				)}
				<Link className="quiet" href="/docs">
					Read the docs
				</Link>
			</div>
			{on === "linux" && (
				<code ref={command} className="command">
					{INSTALL}
				</code>
			)}
			<span className="fineprint">
				{on === "linux" && (
					<>
						Free. Paste it in a terminal on Linux x86_64. Or{" "}
						<a className="quiet" href={DMG}>
							download for Mac
						</a>
						.
					</>
				)}
				{on === "mac" && (
					<>
						Free. Needs an Apple silicon Mac. Or{" "}
						<Link className="quiet" href="/docs#install">
							install on Linux
						</Link>
						.
					</>
				)}
				{on === "windows" && (
					<>
						Free for Apple silicon Macs and{" "}
						<Link className="quiet" href="/docs#install">
							Linux
						</Link>
						. There is no Windows build yet.
					</>
				)}
			</span>
		</>
	);
}
