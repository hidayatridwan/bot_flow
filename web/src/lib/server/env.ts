import { dev } from '$app/environment';
import { env } from '$env/dynamic/private';

/**
 * `dynamic`, not `static`: adapter-node builds one artifact that must be re-pointable at a different
 * API without a rebuild.
 *
 * These are functions, not top-level consts, on purpose. A top-level `required('API_BASE_URL')` is
 * evaluated while Vite walks the module graph during `build`, so it would fail any CI that has no
 * `.env` — despite nothing actually calling the API at build time.
 *
 * The cost of that laziness is what [`assertRuntimeEnv`] exists to pay back: a required variable
 * that is only read on first use fails on the first *request*, not at startup.
 */

function required(key: string): string {
	const value = env[key];
	if (!value)
		throw new Error(`Missing required env var ${key}. Copy web/.env.example to web/.env.`);
	return value;
}

/**
 * A positive integer from the environment, or the default.
 *
 * `Number()` alone accepts `'30d'` and yields `NaN`, which then becomes an `AbortSignal.timeout(NaN)`
 * or a cookie `maxAge` of `NaN` — a malformed cookie the browser drops, so sessions simply stop
 * persisting and nothing anywhere says why. Failing loudly at boot beats that.
 */
function positiveInt(key: string, fallback: number): number {
	const raw = env[key];
	if (raw === undefined || raw === '') return fallback;
	const n = Number(raw);
	if (!Number.isFinite(n) || n <= 0) {
		const unit = key.endsWith('_MS') ? 'milliseconds' : 'seconds';
		throw new Error(`${key} must be a positive number of ${unit}, got ${JSON.stringify(raw)}`);
	}
	return n;
}

/**
 * Check everything the server needs, **at startup rather than on the first request**.
 *
 * Called from `hooks.server.ts`, guarded by `!building` so it never runs during `vite build` — the
 * reason the getters above are lazy in the first place.
 *
 * Why this is worth a function of its own: `API_BASE_URL` was only read when a request needed it, so
 * a deployment missing it **started, bound its port, passed a TCP healthcheck, and then 500'd every
 * page**. An orchestrator would have called that healthy and moved on. A process that refuses to
 * start is a deploy that visibly fails.
 */
export function assertRuntimeEnv(): void {
	apiBaseUrl();
	sessionTtlSeconds();
	apiTimeoutMs();
	askTimeoutMs();

	if (dev) return;

	// **The variable whose absence breaks production hardest, and the one nothing enforced.**
	//
	// adapter-node derives `url.origin` from the incoming request. Behind a TLS-terminating proxy the
	// app sees plain HTTP, so `url.origin` is `http://app.example.com` while the browser sends
	// `Origin: https://app.example.com`. That mismatch fails SvelteKit's own `csrf.checkOrigin` *and*
	// both hand-rolled guards in `documents/upload-url/+server.ts` and `playground/ask/+server.ts` —
	// so every form post, every upload and every playground question returns 403. The app looks
	// deployed and is unusable, and nothing in any log names the cause.
	//
	// `PROTOCOL_HEADER` + `HOST_HEADER` are adapter-node's other supported answer (trusting
	// `X-Forwarded-Proto`/`X-Forwarded-Host`), so either arrangement satisfies this.
	const hasOrigin = !!env.ORIGIN;
	const hasForwardedHeaders = !!env.PROTOCOL_HEADER && !!env.HOST_HEADER;
	if (!hasOrigin && !hasForwardedHeaders) {
		throw new Error(
			'Missing ORIGIN. Behind a TLS-terminating proxy adapter-node cannot infer the public ' +
				'origin, so every form post and both JSON endpoints would fail their CSRF/Origin check ' +
				'with a 403. Set ORIGIN=https://your.host, or set both PROTOCOL_HEADER and HOST_HEADER ' +
				'to trust your proxy. See web/.env.example.'
		);
	}
}

export const apiBaseUrl = (): string => required('API_BASE_URL').replace(/\/+$/, '');

/**
 * The API URL that goes into the widget embed snippet — the URL a *tenant's visitors' browsers* must
 * reach, which is not necessarily the one this server uses (`API_BASE_URL` may be an internal address
 * like `http://api:3000`).
 *
 * This does not breach the "nothing in PUBLIC_*" rule above: the browser still receives no API config
 * for its own use. This is display text the tenant copies into their own site.
 */
export const widgetApiBaseUrl = (): string =>
	(env.WIDGET_API_BASE_URL || apiBaseUrl()).replace(/\/+$/, '');

/** Mirrors the API's SESSION_TTL_SECS. Only sets the cookie max-age; the session row is the authority. */
export const sessionTtlSeconds = (): number => positiveInt('SESSION_TTL_SECS', 2_592_000);

export const apiTimeoutMs = (): number => positiveInt('API_TIMEOUT_MS', 10_000);

/**
 * The ceiling on one `/ask/stream` request, headers *and* body.
 *
 * Not `API_TIMEOUT_MS`, and the gap is not a rounding error. That budget is sized for a database read
 * and must keep protecting `/documents` and `/keys` from a hung API. An ask is a different animal:
 * `ask_stream` runs the query rewrite (a full LLM call) *and* retrieval (an embedding call + Qdrant)
 * **before** it constructs the SSE response, so the first byte does not arrive until two model calls
 * have finished. Ten seconds would kill healthy requests routinely.
 *
 * **This is a backstop, not the authority, and it must stay larger than the API's own ceiling.**
 * `STREAM_DEADLINE` in `handlers.rs` is 300s, and it is deliberately graceful: when it fires the API
 * emits a normal `done` and *persists what arrived*, so the user keeps the prose they watched appear.
 * If this value were the smaller of the two it would fire first and abort the fetch — losing the
 * answer **and** the turn, which is precisely the outcome the API's design goes out of its way to
 * avoid. So: 330s, comfortably above 300s.
 *
 * That ordering is a cross-codebase invariant with no compiler behind it, exactly like
 * `SESSION_TTL_SECS`. Raising `STREAM_DEADLINE` without raising this silently reintroduces the bug.
 *
 * (This comment described a world where the API had no timeouts at all and `max_tokens` was 512.
 * Both stopped being true — invariant 28 gave every gateway call a bound, and phase 15 gave the
 * stream a wall clock — and the stale version had this value sitting *below* the API's ceiling.)
 */
export const askTimeoutMs = (): number => positiveInt('ASK_TIMEOUT_MS', 330_000);
