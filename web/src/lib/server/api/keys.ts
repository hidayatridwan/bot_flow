import type { ApiClient } from './client';

/**
 * The wire shape for `/auth/keys*`. snake_case stops here, as in `auth.ts` and `documents.ts`.
 */

export interface ApiKeyDto {
	/** Not secret — it is the handle for revoke/patch. The raw key is never returned by any list. */
	key_hash: string;
	kind: string;
	label: string;
	allowed_origins: string[];
	created_at: string;
}

export interface ListKeysResponse {
	keys: ApiKeyDto[];
}

export interface CreateKeyBody {
	kind: 'secret' | 'publishable';
	label?: string;
	allowed_origins?: string[];
}

export interface CreateKeyResponse {
	kind: string;
	allowed_origins: string[];
	/** The one-time reveal. Only its hash is stored; if we drop it, it is gone forever. */
	api_key: string;
	note: string;
}

export interface UpdateKeyBody {
	allowed_origins: string[];
}

export interface UpdateKeyResponse {
	key_hash: string;
	kind: string;
	allowed_origins: string[];
}

/** Metadata only — never the raw key. */
export const listKeys = (client: ApiClient) => client.get<ListKeysResponse>('/auth/keys');

export const createKey = (client: ApiClient, body: CreateKeyBody) =>
	client.post<CreateKeyResponse>('/auth/keys', body);

/** Changes the allow-list, and only that. `kind` and the hash are immutable server-side. */
export const updateKeyOrigins = (client: ApiClient, keyHash: string, body: UpdateKeyBody) =>
	client.request<UpdateKeyResponse>('PATCH', `/auth/keys/${keyHash}`, body);

export const revokeKey = (client: ApiClient, keyHash: string) =>
	client.del<void>(`/auth/keys/${keyHash}`);
