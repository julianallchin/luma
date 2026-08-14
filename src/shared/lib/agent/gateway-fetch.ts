/**
 * The Vercel AI Gateway answers preflights with a narrow
 * `Access-Control-Allow-Headers` list, but the Anthropic SDK that pi-ai uses
 * for `anthropic-messages` always attaches `user-agent`, `anthropic-version`,
 * `anthropic-dangerous-direct-browser-access` and a pile of `x-stainless-*`
 * headers. WKWebView puts all of them in `Access-Control-Request-Headers`, the
 * gateway refuses the preflight, and the request never leaves the webview.
 *
 * Strip everything the gateway does not allow before the browser builds its
 * preflight. The list below is exactly what the gateway advertises, plus the
 * CORS-safelisted request headers.
 *
 * `x-api-key` — how the Anthropic SDK carries a plain API key — is one of the
 * headers the gateway will not accept cross-origin, so it is rewritten to the
 * `Authorization: Bearer` form the gateway does allow.
 */

const GATEWAY_HOST = "ai-gateway.vercel.sh";

const ALLOWED_HEADERS = new Set([
	"accept",
	"accept-language",
	"content-language",
	"content-type",
	"authorization",
	"anthropic-beta",
	"http-referer",
	"x-title",
	"x-vercel-ai-gateway-team",
	"ai-gateway-auth-method",
	"ai-gateway-protocol-version",
	"ai-model-id",
	"ai-language-model-id",
	"ai-language-model-specification-version",
	"ai-image-model-specification-version",
	"ai-embedding-model-specification-version",
	"ai-language-model-streaming",
	"ai-reporting-tags",
	"ai-reporting-user",
]);

function isGatewayUrl(url: string): boolean {
	try {
		return new URL(url).hostname === GATEWAY_HOST;
	} catch {
		return false;
	}
}

let installed = false;

/** Idempotently wrap `globalThis.fetch` so gateway requests survive preflight. */
export function installGatewayFetch(): void {
	if (installed) return;
	installed = true;

	const original = globalThis.fetch.bind(globalThis);

	globalThis.fetch = (input, init) => {
		const url = input instanceof Request ? input.url : String(input);
		if (!isGatewayUrl(url)) return original(input, init);

		const request = new Request(input, init);

		const apiKey = request.headers.get("x-api-key");
		if (apiKey && !request.headers.has("authorization")) {
			request.headers.set("authorization", `Bearer ${apiKey}`);
		}

		for (const name of [...request.headers.keys()]) {
			if (!ALLOWED_HEADERS.has(name.toLowerCase())) {
				request.headers.delete(name);
			}
		}
		return original(request);
	};
}
