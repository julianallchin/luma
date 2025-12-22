"use client";

import React, { useState, useEffect, useRef } from "react";
import {
  ArrowRight,
  Cpu,
  Music,
  Layers,
  MoveUpRight,
  Workflow,
} from "lucide-react";

/* --- CORE SYSTEMS & HOOKS --- */

const useMousePosition = () => {
  const [mousePosition, setMousePosition] = useState({ x: 0, y: 0 });
  useEffect(() => {
    const updateMousePosition = (e: MouseEvent) => {
      setMousePosition({ x: e.clientX, y: e.clientY });
    };
    window.addEventListener("mousemove", updateMousePosition);
    return () => window.removeEventListener("mousemove", updateMousePosition);
  }, []);
  return mousePosition;
};

const useScrollProgress = () => {
  const [progress, setProgress] = useState(0);
  useEffect(() => {
    const handleScroll = () => {
      const totalScroll = document.documentElement.scrollTop;
      const windowHeight =
        document.documentElement.scrollHeight -
        document.documentElement.clientHeight;
      const scroll = totalScroll / windowHeight;
      setProgress(scroll);
    };
    window.addEventListener("scroll", handleScroll);
    return () => window.removeEventListener("scroll", handleScroll);
  }, []);
  return progress;
};

/* --- AVANT-GARDE UI COMPONENTS --- */

const GrainOverlay = () => (
  <div className="fixed inset-0 pointer-events-none opacity-[0.03] z-[10] mix-blend-overlay">
    <div
      className="w-full h-full bg-repeat animate-grain"
      style={{
        backgroundImage:
          'url("data:image/svg+xml,%3Csvg viewBox=%220 0 200 200%22 xmlns=%22http://www.w3.org/2000/svg%22%3E%3Cfilter id=%22noiseFilter%22%3E%3CfeTurbulence type=%22fractalNoise%22 baseFrequency=%220.65%22 numOctaves=%223%22 stitchTiles=%22stitch%22/%3E%3C/filter%3E%3Crect width=%22100%25%22 height=%22100%25%22 filter=%22url(%23noiseFilter)%22/%3E%3C/svg%3E")',
      }}
    />
  </div>
);

const RevealText = ({
  children,
  delay = 0,
}: {
  children: React.ReactNode;
  delay?: number;
}) => {
  const [isVisible, setIsVisible] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) setTimeout(() => setIsVisible(true), delay);
      },
      { threshold: 0.1 }
    );
    if (ref.current) observer.observe(ref.current);
    return () => observer.disconnect();
  }, [delay]);

  return (
    <div ref={ref} className="overflow-hidden">
      <div
        className={`transform transition-transform duration-1000 cubic-bezier(0.16, 1, 0.3, 1) ${
          isVisible ? "translate-y-0" : "translate-y-full"
        }`}
      >
        {children}
      </div>
    </div>
  );
};

/* --- SECTIONS --- */

const Navigation = () => (
  <div className="fixed top-0 left-0 w-full p-8 flex justify-center items-center z-50 backdrop-blur-md bg-black/50 border-b border-white/10 text-white">
    <div className="flex gap-12 font-mono text-xs tracking-widest hidden md:flex">
      {[
        { label: "MANIFESTO", href: "#manifesto" },
        { label: "ENGINE", href: "#engine" },
        { label: "DOCS", href: "/docs" },
      ].map((item) => (
        <a
          key={item.label}
          href={item.href}
          className="group relative overflow-hidden h-4 cursor-pointer"
        >
          <span className="block group-hover:-translate-y-full transition-transform duration-300">
            {item.label}
          </span>
          <span className="absolute top-full left-0 block group-hover:-translate-y-full transition-transform duration-300 text-indigo-400">
            [OPEN]
          </span>
        </a>
      ))}
    </div>
  </div>
);

const Hero = () => {
  const { y } = useMousePosition();

  return (
    <section className="h-screen w-full relative flex items-center justify-center overflow-hidden bg-black text-white">
      {/* Dynamic Background Grid */}
      <div
        className="absolute inset-0 z-0 opacity-20"
        style={{
          backgroundImage:
            "linear-gradient(rgba(255,255,255,0.1) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.1) 1px, transparent 1px)",
          backgroundSize: "100px 100px",
          transform: `perspective(1000px) rotateX(60deg) translateY(${
            y * 0.1
          }px) translateZ(-200px)`,
        }}
      />

      {/* Massive Typography */}
      <div className="relative z-10 w-full px-4 md:px-12 mix-blend-difference">
        <div className="border-t border-white/20 w-full mb-4" />
        <h1 className="text-[12vw] leading-[0.8] font-bold tracking-tighter uppercase text-center md:text-left">
          <RevealText delay={0}>Operating</RevealText>
          <RevealText delay={100}>System</RevealText>
          <div className="flex items-center justify-between">
            <RevealText delay={200}>
              <span
                className="text-transparent"
                style={{ WebkitTextStroke: "2px white" }}
              >
                For Light
              </span>
            </RevealText>
            <div className="hidden md:block w-32 h-32 border border-white/30 rounded-full animate-spin-slow p-2">
              <div className="w-full h-full border border-dashed border-white/30 rounded-full" />
            </div>
          </div>
        </h1>
        <div className="border-b border-white/20 w-full mt-8" />
      </div>

      <div className="absolute bottom-12 left-12 text-xs font-mono max-w-xs leading-relaxed opacity-60 hidden md:block">
        Write once, perform anywhere. <br />
        Separating creative intent from <br />
        hardware implementation.
      </div>
    </section>
  );
};

const Manifesto = () => {
  return (
    <section
      id="manifesto"
      className="relative bg-neutral-950 text-neutral-200 py-32 overflow-hidden"
    >
      <div className="absolute top-0 right-0 p-24 opacity-10 font-mono text-9xl font-bold [writing-mode:vertical-rl] pointer-events-none select-none">
        INTENT
      </div>

      <div className="max-w-7xl mx-auto px-6 relative z-10">
        <div className="grid grid-cols-1 md:grid-cols-12 gap-12">
          <div className="md:col-span-5 relative">
            <div className="sticky top-32">
              <h2 className="text-6xl font-bold tracking-tighter mb-8 text-white">
                Kill The <br />
                <span className="text-indigo-500">Console.</span>
              </h2>
              <p className="text-xl leading-relaxed text-neutral-400">
                Traditional lighting is brittle. It records hardware
                instructions. Luma records{" "}
                <span className="text-white font-bold">Semantic Intent</span>.
              </p>

              <div className="mt-12 p-6 border border-neutral-800 bg-black font-mono text-sm relative group overflow-hidden">
                <div className="absolute inset-0 bg-indigo-900/10 translate-y-full group-hover:translate-y-0 transition-transform duration-500" />
                <div className="relative z-10">
                  <div className="flex justify-between text-xs text-neutral-500 mb-4 border-b border-neutral-800 pb-2">
                    <span>TRANSLATION_LAYER</span>
                    <span>ACTIVE</span>
                  </div>
                  <p className="text-indigo-400 mb-1">{`> Input: "Red Pulse on Kick"`}</p>
                  <p className="text-neutral-500 mb-1">{`> Analyzing Venue Fixtures...`}</p>
                  <p className="text-neutral-500 mb-1">{`> Found: 24x Sharpy, 12x Atomic`}</p>
                  <p className="text-white mt-2">{`> Output: Generative Mapping Applied.`}</p>
                </div>
              </div>
            </div>
          </div>

          <div className="md:col-span-7 space-y-32 pt-24 md:pt-0">
            {[
              {
                title: "Abstraction",
                desc: "Hardware blind. Design for the song, not the venue.",
              },
              {
                title: "Procedural",
                desc: "Mathematical behaviors. No more manual keyframing.",
              },
              {
                title: "AI Ready",
                desc: "Structured data for the next generation of models.",
              },
            ].map((item, i) => (
              <div key={i} className="group border-t border-neutral-800 pt-8">
                <span className="text-xs font-mono text-indigo-500 mb-4 block">
                  0{i + 1}
                </span>
                <h3 className="text-4xl md:text-5xl font-bold mb-4 group-hover:translate-x-4 transition-transform duration-300">
                  {item.title}
                </h3>
                <p className="text-neutral-500 text-lg max-w-md group-hover:text-white transition-colors duration-300">
                  {item.desc}
                </p>
              </div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
};

const EngineFeatures = () => {
  return (
    <section
      id="engine"
      className="bg-white text-black py-24 relative overflow-hidden"
    >
      {/* Marquee */}
      <div className="w-full overflow-hidden border-y border-black mb-24 py-2">
        <div className="whitespace-nowrap animate-marquee flex gap-8">
          {[...Array(10)].map((_, i) => (
            <span
              key={i}
              className="text-8xl font-black uppercase tracking-tighter opacity-10"
            >
              Audio Reactive Engine &mdash;
            </span>
          ))}
        </div>
      </div>

      <div className="max-w-7xl mx-auto px-6 grid grid-cols-1 md:grid-cols-2 gap-px bg-neutral-200 border border-neutral-200">
        {[
          {
            icon: <Workflow size={32} />,
            title: "Node Graph",
            desc: "Reactive flow architecture using Rust-based computation.",
          },
          {
            icon: <Music size={32} />,
            title: "Stem Separation",
            desc: "Isolates drums, bass, and vocals in real-time.",
          },
          {
            icon: <Layers size={32} />,
            title: "Compositor",
            desc: "Photoshop-style layer blending for light.",
          },
          {
            icon: <Cpu size={32} />,
            title: "The Stack",
            desc: "Rust core + Tauri frontend. Native speed.",
          },
        ].map((feature, i) => (
          <div
            key={i}
            className="bg-white p-12 hover:invert transition-all duration-300 group"
          >
            <div className="mb-12 opacity-50 group-hover:opacity-100 transition-opacity">
              {feature.icon}
            </div>
            <h4 className="text-3xl font-bold mb-4">{feature.title}</h4>
            <p className="text-lg opacity-60 leading-relaxed">{feature.desc}</p>
            <a
              href="/docs"
              className="mt-8 flex items-center gap-2 text-xs font-mono uppercase opacity-0 group-hover:opacity-100 transition-opacity hover:text-indigo-600"
            >
              <span>Read Docs</span>
              <MoveUpRight size={12} />
            </a>
          </div>
        ))}
      </div>
    </section>
  );
};

const Vision = () => (
  <section className="bg-black text-white min-h-screen flex items-center relative overflow-hidden px-6">
    <div className="absolute inset-0 bg-[url('https://grainy-gradients.vercel.app/noise.svg')] opacity-20 brightness-100 contrast-150"></div>

    <div className="max-w-5xl mx-auto w-full relative z-10">
      <div className="flex flex-col md:flex-row items-end gap-12 md:gap-24">
        <h2 className="text-7xl md:text-9xl font-bold tracking-tighter leading-[0.8]">
          <span className="block text-indigo-600">AI</span>
          <span className="block opacity-50">NATIVE</span>
          <span className="block">CORE</span>
        </h2>

        <div className="max-w-md pb-4">
          <p className="text-xl md:text-2xl font-light leading-relaxed mb-8">
            Luma isn&apos;t just a tool. It&apos;s a dataset generator. We are
            building the model that listens.
          </p>
          <a
            href="/docs"
            className="group flex items-center gap-4 text-left hover:opacity-80 transition-opacity"
          >
            <div className="w-16 h-16 bg-white rounded-full flex items-center justify-center text-black">
              <ArrowRight className="group-hover:translate-x-1 transition-transform" />
            </div>
            <span className="font-mono text-sm tracking-widest uppercase">
              Read The Whitepaper
            </span>
          </a>
        </div>
      </div>
    </div>
  </section>
);

const Footer = () => (
  <footer className="bg-black text-neutral-500 py-12 px-6 border-t border-neutral-900 font-mono text-xs">
    <div className="max-w-7xl mx-auto flex flex-col md:flex-row justify-between items-end gap-8">
      <div>
        <div className="text-white text-2xl font-bold tracking-tighter mb-4">
          LUMA
        </div>
        <div className="max-w-xs space-y-2">
          <p>San Francisco, CA</p>
          <p>Designed for the dark.</p>
        </div>
      </div>
    </div>
    <div className="max-w-7xl mx-auto mt-12 pt-8 border-t border-neutral-900 flex justify-between opacity-40">
      <span>(C) 2024 LUMA SYSTEMS</span>
      <span>ALL RIGHTS RESERVED</span>
    </div>
  </footer>
);

/* --- MAIN PAGE --- */

export default function LandingPage() {
  return (
    <div className="bg-black min-h-screen">
      <GrainOverlay />
      <Navigation />

      <main>
        <Hero />
        <Manifesto />
        <EngineFeatures />
        <Vision />
      </main>

      <Footer />
    </div>
  );
}