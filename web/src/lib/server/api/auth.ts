import type { ApiClient } from './client';

/**
 * The wire shape. snake_case stops here: everything above this file speaks `SessionUser`. That
 * boundary is why a rename in the Rust API would touch exactly one file.
 */

export interface RegisterBody {
	email: string;
	password: string;
	tenant_name: string;
	slug?: string;
}

export interface RegisterResponse {
	session_token: string;
	tenant_id: string;
	/** The one-time `sk_` reveal. Only its hash is stored; if we drop it, it is gone forever. */
	api_key: string;
	note: string;
}

export interface LoginBody {
	email: string;
	password: string;
}

export interface LoginResponse {
	session_token: string;
	tenant_id: string;
}

export interface MeResponse {
	account: { email: string };
	tenant: { id: string; name: string };
}

export const register = (client: ApiClient, body: RegisterBody) =>
	client.post<RegisterResponse>('/auth/register', body);

export const login = (client: ApiClient, body: LoginBody) =>
	client.post<LoginResponse>('/auth/login', body);

/** 204, empty body. */
export const logout = (client: ApiClient) => client.post<void>('/auth/logout');

export const me = (client: ApiClient) => client.get<MeResponse>('/auth/me');
