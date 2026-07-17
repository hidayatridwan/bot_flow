import { env } from '$env/dynamic/private';

/**
 * `dynamic`, not `static`: adapter-node builds one artifact that must be re-pointable at a different
 * API without a rebuild.
 *
 * These are functions, not top-level consts, on purpose. A top-level `required('API_BASE_URL')` is
 * evaluated while Vite walks the module graph during `build`, so it would fail any CI that has no
 * `.env` — despite nothing actually calling the API at build time.
 */

function required(key: string): string {
	const value = env[key];
	if (!value)
		throw new Error(`Missing required env var ${key}. Copy web/.env.example to web/.env.`);
	return value;
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
export const sessionTtlSeconds = (): number => Number(env.SESSION_TTL_SECS ?? 2_592_000);

export const apiTimeoutMs = (): number => Number(env.API_TIMEOUT_MS ?? 10_000);

/**
 * The ceiling on one `/ask/stream` request, headers *and* body.
 *
 * Not `API_TIMEOUT_MS`, and the gap is not a rounding error. That budget is sized for a database read
 * and must keep protecting `/documents` and `/keys` from a hung API. An ask is a different animal:
 * `ask_stream` runs the query rewrite (a full LLM call) *and* retrieval (an embedding call + Qdrant)
 * **before** it constructs the SSE response, so the first byte does not arrive until two model calls
 * have finished. Ten seconds would kill healthy requests routinely.
 *
 * The default is generous because this is the **only** timeout in the whole chain. The API builds its
 * LLM and embedding clients with `reqwest::Client::new()` and no `.timeout()`, so nothing upstream
 * bounds a hung gateway. The one thing that does bound a *legitimate* answer is `max_tokens: 512` in
 * `llm.rs` — at a slow-but-real 5 tokens/sec that is ~100s of streaming, plus the two calls above.
 * Anything tighter truncates good answers; the value is env-tunable for deployments that know their
 * gateway is faster.
 */
export const askTimeoutMs = (): number => Number(env.ASK_TIMEOUT_MS ?? 120_000);
