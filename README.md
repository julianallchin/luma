# Luma

[![CI](https://github.com/julianallchin/luma/actions/workflows/ci.yml/badge.svg)](https://github.com/julianallchin/luma/actions/workflows/ci.yml)

Semantic lighting control system for creating portable light shows that work across any venue.

## Project Structure

- **`src/`** - Frontend React/TypeScript application with Zustand state management and React Flow for visual pattern editing
- **`src-tauri/`** - Backend Rust/Tauri application with SQLite databases, audio processing workers, and pattern execution engine
- **`experiments/`** - Research code, audio analysis notebooks, and test data including sample songs and stem separations
- **`projects/`** - Luma project files (.luma) containing venue-specific pattern implementations
- **`dist/`** - Built application output for distribution
- **`public/`** - Static assets and icons

## Getting Started

1. Install Bun: `curl -fsSL https://bun.sh/install | bash` (macOS/Linux) or visit https://bun.sh for Windows.

2. Install git dependencies recursively: `git submodule update --init --recursive`

3. Install Rust via their installer: https://rust-lang.org/tools/install/

4. Install JavaScript dependencies: `bun install`

5. Start development server: `bun run tauri dev`

> You might have issues with the python version as that isnt really fleshed out yet.
