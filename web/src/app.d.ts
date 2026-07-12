// See https://svelte.dev/docs/kit/types#app.d.ts
// for information about these interfaces
import type { SessionUser } from '$lib/types/auth';

declare global {
	namespace App {
		interface Locals {
			/**
			 * The raw `sess_` token. Server-only — NEVER return this from a load function.
			 * Kept separate from `user` so that returning the identity cannot leak the credential.
			 */
			session: { token: string } | null;
			user: SessionUser | null;
		}
		interface PageData {
			user?: SessionUser | null;
		}
	}
}

export {};
