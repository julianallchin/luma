/**
 * Cache for converted colors to avoid repeated DOM operations.
 * Key is the CSS variable name, value is the computed RGB string.
 */
const colorCache = new Map<string, string>();

/**
 * Convert oklch color to rgb string.
 * oklch(L C H) where L is lightness (0-1), C is chroma, H is hue in degrees.
 */
function oklchToRgb(l: number, c: number, h: number): string {
	// Convert oklch to oklab (polar to cartesian)
	const hRad = (h * Math.PI) / 180;
	const a = c * Math.cos(hRad);
	const b = c * Math.sin(hRad);

	// Convert oklab to linear sRGB
	// Using the standard oklab -> linear sRGB matrix
	const l_ = l + 0.3963377774 * a + 0.2158037573 * b;
	const m_ = l - 0.1055613458 * a - 0.0638541728 * b;
	const s_ = l - 0.0894841775 * a - 1.291485548 * b;

	const l3 = l_ * l_ * l_;
	const m3 = m_ * m_ * m_;
	const s3 = s_ * s_ * s_;

	// Linear sRGB
	const r = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3;
	const g = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3;
	const bl = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.707614701 * s3;

	// Convert linear sRGB to sRGB (gamma correction)
	const toSrgb = (x: number) => {
		if (x <= 0) return 0;
		if (x >= 1) return 255;
		return Math.round(
			255 * (x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055),
		);
	};

	return `rgb(${toSrgb(r)}, ${toSrgb(g)}, ${toSrgb(bl)})`;
}

/**
 * Parse a color string and convert to rgb format.
 * Handles oklch(), rgb(), rgba(), and other formats.
 */
function parseToRgb(colorStr: string): string {
	// Check for oklch format: oklch(L C H) or oklch(L C H / alpha)
	const oklchMatch = colorStr.match(
		/oklch\(([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*[\d.]+)?\)/,
	);
	if (oklchMatch) {
		const l = parseFloat(oklchMatch[1]);
		const c = parseFloat(oklchMatch[2]);
		const h = parseFloat(oklchMatch[3]);
		return oklchToRgb(l, c, h);
	}

	// Check for rgb/rgba format - already in correct format
	if (colorStr.startsWith("rgb")) {
		return colorStr;
	}

	// Fallback - return black
	return "rgb(0, 0, 0)";
}

/**
 * Get RGB color from CSS variable for use in canvas.
 * Handles oklch() and other color formats by converting to RGB.
 */
export function getCanvasColor(cssVar: string): string {
	// Check cache first
	const cached = colorCache.get(cssVar);
	if (cached) return cached;

	const value = getComputedStyle(document.documentElement)
		.getPropertyValue(cssVar)
		.trim();

	if (!value) return "rgb(0, 0, 0)";

	// Convert to rgb
	const result = parseToRgb(value);
	colorCache.set(cssVar, result);

	return result;
}

/**
 * Parse RGB(A) values from a computed color string.
 * Handles both rgb() and rgba() formats.
 */
function parseRgbValues(
	color: string,
): { r: number; g: number; b: number } | null {
	// Match rgb(r, g, b) or rgba(r, g, b, a)
	const match = color.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*[\d.]+)?\)/);
	if (match) {
		return {
			r: parseInt(match[1], 10),
			g: parseInt(match[2], 10),
			b: parseInt(match[3], 10),
		};
	}
	return null;
}

/**
 * Get RGB color with alpha transparency from CSS variable.
 */
export function getCanvasColorRgba(cssVar: string, alpha: number): string {
	const color = getCanvasColor(cssVar);
	const rgb = parseRgbValues(color);

	if (rgb) {
		return `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${alpha})`;
	}

	return `rgba(0, 0, 0, ${alpha})`;
}

/**
 * Clear the color cache. Call this when theme changes.
 */
export function clearColorCache(): void {
	colorCache.clear();
}
